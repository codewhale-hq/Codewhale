//! Independent, object-safe capability shapes for staged command migration.
//!
//! FEAT-014 publishes these interfaces without implementing them for the TUI
//! or changing an existing command. Later work adopts them inside
//! `codewhale-tui` one command group at a time. Only after every group uses
//! these shapes will groups move physically into a commands crate.

pub use crate::config_policy::{CommandConfigStatusContext, CommandPermissionsContext};

use std::path::{Path, PathBuf};

mod session_structcopy;
pub use session_structcopy::*;

mod debug_operations;
pub mod debug_receipts;
pub use debug_operations::*;
pub use debug_receipts::*;

mod diagnostics_report;
mod diagnostics_tools;
pub use diagnostics_report::*;
pub use diagnostics_tools::*;

use codewhale_protocol::request::{Message, SystemPrompt};
use serde_json::Value;

use crate::types::{CommandApprovalMode, CommandCurrency, CommandMode, CommandProviderId};

/// Session identity, messages, queue operations, and token totals.
pub trait CommandSessionContext {
    fn session_id(&self) -> Option<String>;
    fn api_messages(&self) -> Vec<Message>;
    fn add_message(&mut self, message: Message);
    fn queued_message_count(&self) -> usize;
    fn remove_queued_message(&mut self, index: usize) -> Result<(), String>;
    fn total_tokens(&self) -> u64;
}

/// Model selection, provider identity, and fallback chain.
///
/// Slice 4 dropped the `reasoning_effort()` facet: reasoning preference is
/// owned by `codewhale_tui::reasoning_preference::ReasoningEffort` and no
/// command group consumed the duplicated boundary enum.
pub trait CommandModelContext {
    fn current_model(&self) -> String;
    fn auto_model(&self) -> bool;
    fn set_model_selection(&mut self, model: String, provider: Option<CommandProviderId>);
    fn provider_identity(&self) -> Option<CommandProviderId>;
    fn fallback_chain(&self) -> Vec<CommandProviderId>;
}

/// Cost display and accounting operations.
pub trait CommandCostContext {
    fn display_currency(&self) -> CommandCurrency;
    fn session_cost_for_currency(&self, currency: CommandCurrency) -> f64;
    fn subagent_cost_for_currency(&self, currency: CommandCurrency) -> f64;
    fn accrue_cost_estimate(&mut self, amount: f64, currency: CommandCurrency);
    fn record_turn_cost(
        &mut self,
        amount: f64,
        currency: CommandCurrency,
        route_receipt: Option<String>,
    );
}

/// Operating mode, approval posture, shell access, and policy lock.
pub trait CommandModePolicyContext {
    fn mode(&self) -> CommandMode;
    fn set_mode(&mut self, mode: CommandMode);
    fn approval_mode(&self) -> CommandApprovalMode;
    fn allow_shell(&self) -> bool;
    fn set_shell_access(&mut self, allow: bool);
    fn policy_locked(&self) -> bool;
}

/// Read access to the effective system prompt.
pub trait CommandSystemPromptContext {
    fn system_prompt(&self) -> Option<SystemPrompt>;
}

/// Active skill identity and skill-cache refresh.
pub trait CommandSkillsContext {
    fn active_skill(&self) -> Option<String>;
    fn active_skill_provenance(&self) -> Option<String>;
    fn refresh_skill_cache(&mut self);
}

/// Workspace path and a bounded serialized work-state snapshot.
pub trait CommandWorkspaceContext {
    fn workspace(&self) -> PathBuf;
    fn work_state_snapshot(&self) -> Result<Option<String>, String>;
    /// Session-aware canonical operation digest. Returns the final user-facing
    /// digest text or a safe explicit error; never a serialized snapshot.
    /// No-active-work and temporary-unavailability semantics are preserved by
    /// the host implementation (FEAT-018 D5).
    fn operation_digest(&mut self) -> Result<String, String>;
}

/// Stable-key translation with named replacements (FEAT-018 D3).
///
/// Message identity uses stable snake_case keys plus named replacements. The
/// TUI host maps those keys to the current catalog and preserves the existing
/// English fallback for intentionally incomplete locale packs. Unknown keys or
/// invalid replacement contracts fail safely and produce a command error; they
/// never panic and never display a raw lookup key.
pub trait CommandPresentationContext {
    /// Resolve a stable message key with its named replacements.
    fn translate(&self, key: &str, replacements: &[(&str, &str)]) -> Result<String, String>;
}

/// Portable receipt for a successful atomic media attachment (FEAT-018 D4).
/// Carries only the information needed for the existing confirmation text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaAttachmentReceipt {
    pub kind: String,
    pub path: std::path::PathBuf,
}

/// Atomic composer/media capability (FEAT-018 D4).
///
/// The host performs media validation and composer insertion as one atomic
/// operation. Rejected, missing, unsupported, corrupt, or oversized media
/// leaves composer state unchanged and returns a safe error. Only portable
/// success information crosses the boundary; composer markup, mutable input
/// text, decoder internals, and TUI types never do.
pub trait CommandMediaContext {
    /// Validate and insert a resolved media path atomically.
    fn attach_media(&mut self, resolved_path: &Path) -> Result<MediaAttachmentReceipt, String>;
}

// ---------------------------------------------------------------------------
// Debug diagnostics (FEAT-029 D3)
// ---------------------------------------------------------------------------

/// Only the provider identity and support decision consumed by `/balance`.
/// The adapter derives support from the authoritative provider policy; the
/// portable handler decides whether to emit `FetchBalance` or the original text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DebugBalanceProjection {
    pub provider_display_name: String,
    pub supports_balance_api: bool,
}

/// Source text consumed by `/system`; retain Text/Blocks/None separately
/// so the portable handler alone owns the separators and empty-state text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DebugSystemPrompt {
    None,
    Text(String),
    Blocks(Vec<String>),
}

/// Only the data read by the `/system` renderer, with the host's mode label.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DebugSystemProjection {
    pub mode_label: String,
    pub prompt: DebugSystemPrompt,
}

/// Published usage telemetry remains optional: absence is not zero.
#[derive(Debug, Clone, PartialEq)]
pub struct DebugTokenProjection {
    pub active_context_used: usize,
    pub context_window: u32,
    pub last_input: Option<u32>,
    pub last_output: Option<u32>,
    pub cache_hit: Option<u32>,
    pub cache_miss: Option<u32>,
    pub total_tokens: u64,
    pub cache_write_tokens: u64,
    pub api_message_count: usize,
    pub chat_message_count: usize,
    pub model: String,
    pub cost: DebugCostProjection,
}

/// Already-authoritative monetary values and bounded route attribution.
/// Calculation and price/source selection stay with the TUI host; formatting,
/// ordering and coverage wording belong to the portable handlers.
#[derive(Debug, Clone, PartialEq)]
pub struct DebugCostProjection {
    pub currency: CommandCurrency,
    pub total: f64,
    pub parent_turns: f64,
    pub subagents: f64,
    pub display_floor: f64,
    pub priced_turns: u32,
    pub unpriced_turns: u32,
    pub legacy_coverage_unknown: bool,
    pub user_declared_estimates: bool,
    pub itemized_turns: u32,
    pub route_amounts: Vec<DebugRouteCost>,
    pub turn_history_capacity: usize,
    pub unpriced_reason_labels: Vec<String>,
    pub unpriced_classes: Vec<String>,
    pub pricing_provenances: Vec<String>,
    pub live_pricing_defects: Vec<String>,
    pub unusable_pricing_defects: Vec<String>,
    pub route_receipts: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DebugRouteCost {
    pub route: String,
    pub amount: f64,
}

/// A cache inspection's semantic layer, retaining the same public JSON
/// field order as the baseline without borrowing the client request type.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct DebugPromptLayer {
    pub name: String,
    pub stability: DebugPromptLayerStability,
    pub char_len: usize,
    pub byte_len: usize,
    pub token_estimate: usize,
    pub sha256: String,
    pub tool_result: Option<DebugToolResultInspection>,
    pub turn_meta: Option<DebugTurnMetaInspection>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum DebugPromptLayerStability {
    Static,
    History,
    Dynamic,
}

impl DebugPromptLayerStability {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Static => "static",
            Self::History => "history",
            Self::Dynamic => "dynamic",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct DebugToolResultInspection {
    pub original_chars: usize,
    pub sent_chars: usize,
    pub truncated: bool,
    pub deduplicated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct DebugTurnMetaInspection {
    pub original_chars: usize,
    pub sent_chars: usize,
    pub deduplicated: bool,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct DebugPromptInspection {
    pub base_static_prefix_hash: String,
    pub full_request_prefix_hash: String,
    pub tool_catalog_hash: String,
    pub layers: Vec<DebugPromptLayer>,
}

/// Full key fields are needed for the existing comparison and JSON report;
/// the short hash is computed by the authoritative host hashing function.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct DebugWarmupKey {
    pub provider: String,
    pub model: String,
    pub base_url: String,
    pub static_prefix_hash: String,
    pub tool_catalog_hash: String,
    pub project_pack_hash: String,
    pub skills_hash: String,
}

/// One coherent observation, including previous inspection before any write.
/// Inspect errors occur before this value exists and never update session state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DebugCacheInspectionObservation {
    pub current: DebugPromptInspection,
    pub previous: Option<DebugPromptInspection>,
    pub current_warmup_key: DebugWarmupKey,
    pub last_warmup_key: Option<DebugWarmupKey>,
    pub current_warmup_hash_short: String,
    pub last_warmup_hash_short: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DebugCacheInspectionUnavailable {
    NoConcreteRoute,
    MissingCapturedEndpoint,
}

/// A bounded turn row. Pricing class partition and amount come from the
/// authoritative host, not a second implementation of provider billing.
/// Missing telemetry, missing audit and a measured zero remain distinct.
#[derive(Debug, Clone, PartialEq)]
pub struct DebugCacheTurn {
    pub provider: Option<String>,
    pub provider_identity: Option<String>,
    pub model: Option<String>,
    pub auto_model: bool,
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cache_hit_tokens: Option<u32>,
    pub cache_miss_tokens: Option<u32>,
    pub cache_write_tokens: Option<u32>,
    pub reasoning_tokens: Option<u32>,
    pub reasoning_replay_tokens: Option<u32>,
    pub priced_amount: Option<f64>,
    pub unpriced_reason_key: Option<String>,
    /// Original host enum order for deduplicated /cache note ordering.
    pub unpriced_reason_sort_rank: Option<u8>,
    pub unpriced_classes: Vec<String>,
    pub priced_cache_read: u64,
    pub priced_cache_miss: u64,
    pub priced_cache_write: u64,
    /// Coherent age at observation time; portable display rounds seconds.
    pub age_seconds: u64,
}

/// Prompt-cache hit rates, each labelled by whose requests it covers (#6565).
///
/// `parent` is this conversation's own requests: the footer `cache N%` and it
/// never change meaning. `agents` covers sub-agent and other background
/// requests. `combined` weights both by their tokens. Each is `None` when its
/// requests reported no cache telemetry; no report is never 0%.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DebugCacheRates {
    pub parent: Option<u8>,
    pub agents: Option<u8>,
    pub combined: Option<u8>,
}

impl DebugCacheRates {
    /// `parent 82% · agents 64% · combined 75%` with the given words, or just
    /// `82%` when only the parent reported. `None` when nothing did.
    #[must_use]
    pub fn labelled(&self, parent: &str, agents: &str, combined: &str) -> Option<String> {
        match (self.parent, self.agents) {
            (Some(pct), None) => Some(format!("{pct}%")),
            (None, None) => None,
            _ => Some(
                [
                    (parent, self.parent),
                    (agents, self.agents),
                    (combined, self.combined),
                ]
                .into_iter()
                .filter_map(|(word, pct)| pct.map(|pct| format!("{word} {pct}%")))
                .collect::<Vec<_>>()
                .join(" · "),
            ),
        }
    }
}

/// Shared source for `/cache [count|stats|zones]` branches. One host read
/// preserves ring order, optional telemetry and prefix stability evidence.
#[derive(Debug, Clone, PartialEq)]
pub struct DebugCacheTelemetry {
    pub model: String,
    /// Parent/agent/combined percentages computed once by the host.
    pub session_cache_rates: DebugCacheRates,
    pub history: Vec<DebugCacheTurn>,
    pub history_capacity: usize,
    pub prefix_stability_pct: Option<u32>,
    pub prefix_checks_total: u64,
    pub prefix_change_count: u64,
    pub prefix_drift_count: u64,
    pub prefix_context_updates: u64,
    pub prefix_pin_reason: Option<String>,
    pub prefix_last_miss_reason: Option<String>,
    pub last_prefix_change_desc: Option<String>,
    pub last_pinned_prefix_hash: Option<String>,
    pub api_message_count: usize,
    pub non_system_message_count: usize,
}

/// Narrow, synchronous data boundary for the debug diagnostics slice.
///
/// No concrete provider, App, completed message, or network operation crosses
/// this interface. Add operations only when their live branches require them.
/// Portable handlers consume these facts without concrete host access.
pub trait CommandDebugDiagnosticsContext {
    fn balance_projection(&self) -> DebugBalanceProjection;
    fn system_projection(&self) -> DebugSystemProjection;
    fn token_projection(&self) -> DebugTokenProjection;
    fn cost_projection(&self) -> DebugCostProjection;
    fn cache_telemetry(&self) -> DebugCacheTelemetry;
    /// The report builder stays host-owned; format and JSON serialization
    /// consume only this data-only source map.
    fn context_source_map(&self) -> DebugPromptSourceMap;
    fn prompt_context(&self) -> DebugPromptContext;
    /// None means no prepared snapshot exists; argument validation occurs
    /// only after this check in the portable `/tools` handler.
    fn tool_snapshot(&self) -> Option<DebugToolSnapshot>;
    /// Route resolution and request inspection remain host-owned. This call
    /// must not update the remembered inspection on failure or success.
    fn inspect_cache(
        &self,
    ) -> Result<DebugCacheInspectionObservation, DebugCacheInspectionUnavailable>;
    /// Store the already-observed inspection after portable rendering, without
    /// rebuilding or re-inspecting the request. The baseline also commits on
    /// JSON serialization fallback.
    fn remember_cache_inspection(&mut self, inspection: DebugPromptInspection);
}

// ---------------------------------------------------------------------------
// Project (FEAT-021 D1/D2/D3/D4)
// ---------------------------------------------------------------------------

/// Portable goal status for the project facet (FEAT-021 D1).
///
/// Mirrors the four TUI-owned `tools::goal::GoalStatus` variants without
/// naming the TUI type. The adapter maps host state onto this enum; handlers
/// compare and render it directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ProjectGoalStatus {
    #[default]
    Active,
    Paused,
    Complete,
    Blocked,
}

/// Portable goal projection (FEAT-021 D1).
///
/// Carries the visible goal state, the effective pending-control view, and the
/// session-derived token fallback the live `/goal` handler consumes. Concrete
/// goal-service, session-manager, and `App` types never cross the boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectGoalState {
    /// Visible goal objective.
    pub objective: Option<String>,
    /// Visible goal status.
    pub status: ProjectGoalStatus,
    /// Pause reason label when the goal is paused (already rendered).
    pub pause_reason: Option<String>,
    /// Elapsed seconds from `started_at` when present (host-computed).
    pub started_at_elapsed_seconds: Option<u64>,
    /// Seconds of goal time used (stable budget/elapsed source).
    pub time_used_seconds: u64,
    /// Optional token budget.
    pub token_budget: Option<u32>,
    /// Tokens used by the goal engine.
    pub tokens_used: u64,
    /// Session conversation-token total (fallback when tokens_used == 0).
    pub session_total_tokens: u32,
    /// Goal continuation count.
    pub continuation_count: u32,
    /// Whether pending goal controls are queued (effective-state gate).
    pub pending_controls: bool,
    /// Last-known durable objective (session-derived effective source).
    pub last_known_objective: Option<String>,
    /// Last-known durable status (session-derived effective source).
    pub last_known_status: Option<ProjectGoalStatus>,
    /// Whether the conversation has API messages (bare `/goal` context gate).
    pub conversation_present: bool,
    /// Whether the host is currently loading (idle-hint gate).
    pub is_loading: bool,
    /// Whether the goal continuation loop is waiting (idle-hint gate).
    pub goal_continuation_waiting: bool,
}

/// Host project data for the project command group (FEAT-021 D1).
///
/// Exposes the typed, exact-minimum operations the live project handlers
/// consume: `/lsp` status/set state, `/share` session payload data, and
/// `/goal` goal state including the session-derived effective values.
/// `/init` host data flows through the existing `WORKSPACE` facet (D2), so
/// `/init` destructures exactly `WORKSPACE` (D4) and consumes no
/// project-facet method. All results are contract-owned portable values; implementation
/// errors cross as safe text. The TUI adapter is the only place that touches
/// `App`, `config::config`, the goal service, or the session manager.
pub trait CommandProjectContext {
    /// `/lsp` status: whether LSP diagnostics are enabled.
    fn lsp_enabled(&self) -> bool;
    /// `/lsp` set: enable or disable LSP diagnostics.
    fn lsp_set(&mut self, enabled: bool) -> Result<(), String>;
    /// `/goal` projection: visible and effective goal state.
    fn goal_state(&self) -> ProjectGoalState;
}

// ---------------------------------------------------------------------------
// Memory (FEAT-019 D1/D2/D8/D9)
// ---------------------------------------------------------------------------

/// Portable semantic hit for a native-memory search or get result.
///
/// Carries only the typed location and text the handler consumes for
/// formatting; the TUI-owned `NativeMemoryHit` never crosses the boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryHit {
    pub source: PathBuf,
    pub line_start: usize,
    pub line_end: usize,
    pub text: String,
}

/// Portable native-memory location summary (status operation).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryStatus {
    pub root: PathBuf,
    pub source: PathBuf,
    pub index: PathBuf,
}

/// Portable result of a successful remember operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryRemembered {
    pub source: PathBuf,
    pub line_start: usize,
}

/// Portable import outcome: imported (with destination) or skipped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemoryImportOutcome {
    Imported { destination: PathBuf },
    Skipped,
}

/// Portable get outcome: found hit or explicit not-found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemoryGetOutcome {
    Found(MemoryHit),
    NotFound,
}

/// Portable export payload — the exported memory document itself, never a
/// preformatted command response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryExport {
    pub content: String,
}

/// Portable reindex entry count.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryReindex {
    pub entry_count: usize,
}

/// Zero-field success value for delete operations (D2): the handler already
/// owns the selected scope and needs no additional success data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MemoryDelete;

/// Typed remember target (D9): the handler resolves workspace identity through
/// the workspace facet and passes the resulting typed ID here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemoryRememberTarget {
    Global,
    Workspace { workspace_id: String },
}

/// Typed delete scope for the non-workspace delete method (D8/D9).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryDeleteScope {
    /// Delete every memory entry (global and all workspace scopes).
    All,
    /// Delete only the global scope entries.
    Global,
}

/// Host memory data for the memory command group (FEAT-019 D1).
///
/// Exposes the resolved user-memory file path, the enablement flag, and one
/// typed method per exposed native-memory operation. All results are
/// contract-owned portable values; implementation errors cross as safe text.
/// Workspace-scoped operations take the borrowed workspace path as their first
/// argument (D8); non-workspace operations never receive workspace authority
/// and the facet never captures or retains workspace state internally.
pub trait CommandMemoryContext {
    /// The resolved user-memory file path.
    fn memory_path(&self) -> PathBuf;
    /// Whether the `[memory] enabled` / `DEEPSEEK_MEMORY=on` flag is set.
    fn memory_enabled(&self) -> bool;
    /// Native-memory root, global source, and index paths.
    fn status(&self) -> Result<MemoryStatus, String>;
    /// The native-memory root path.
    fn path(&self) -> Result<PathBuf, String>;
    /// Workspace identity for the given workspace path.
    fn workspace_id(&self, workspace: &Path) -> Result<String, String>;
    /// Workspace-scoped search over the native-memory store.
    fn search(&self, workspace: &Path, query: &str, limit: usize)
    -> Result<Vec<MemoryHit>, String>;
    /// Append a reviewed note to the typed global or workspace target.
    fn remember(
        &self,
        target: MemoryRememberTarget,
        note: &str,
    ) -> Result<MemoryRemembered, String>;
    /// Import legacy memory; distinguishes imported from skipped.
    fn import(&self) -> Result<MemoryImportOutcome, String>;
    /// Workspace-scoped get by entry id; not-found is a typed outcome.
    fn get(&self, workspace: &Path, id: i64) -> Result<MemoryGetOutcome, String>;
    /// Export the native-memory document content.
    fn export(&self) -> Result<MemoryExport, String>;
    /// Reindex the native-memory store; returns the indexed entry count.
    fn reindex(&self) -> Result<MemoryReindex, String>;
    /// Delete all or global scope; never receives workspace authority.
    fn delete(&self, scope: MemoryDeleteScope) -> Result<MemoryDelete, String>;
    /// Delete the given workspace scope; workspace path is the first argument.
    fn delete_workspace(&self, workspace: &Path) -> Result<MemoryDelete, String>;
}

// ---------------------------------------------------------------------------
// Plugin (FEAT-020 D1/D2/D10/D11)
// ---------------------------------------------------------------------------

/// Portable plugin diagnostic level (FEAT-020 D2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginDiagnosticLevel {
    Warning,
    Error,
}

/// Portable plugin diagnostic entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginDiagnostic {
    pub level: PluginDiagnosticLevel,
    pub code: String,
    pub message: String,
    pub path: Option<PathBuf>,
}

/// Portable MCP transport classification for the capability review body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginMcpTransport {
    Stdio,
    Http,
    Invalid,
}

/// Portable MCP server detail for the capability review body (FEAT-020 D2).
///
/// Carries only the semantic fields `render_mcp_inventory` consumes:
/// transport, command/url, argv, cwd, env provenance, timeouts, required,
/// enabled/disabled tool lists, and the enabled flag. Host `McpServerConfig`
/// never crosses the boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginMcpServerDetail {
    pub name: String,
    pub transport: PluginMcpTransport,
    pub command: Option<String>,
    pub argv: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub env: Vec<(String, String)>,
    pub url: Option<String>,
    pub env_headers: Vec<(String, String)>,
    pub bearer_token_env_var: Option<String>,
    pub connect_timeout_secs: Option<u64>,
    pub execute_timeout_secs: Option<u64>,
    pub read_timeout_secs: Option<u64>,
    pub required: bool,
    pub enabled_tools: Vec<String>,
    pub disabled_tools: Vec<String>,
    pub enabled: bool,
}

/// Portable summary of one loaded plugin bundle (list output, FEAT-020 D2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginSummary {
    pub name: String,
    pub id: String,
    pub state_label: String,
    pub scope: String,
    pub trust_status: String,
    pub compatibility: String,
    pub inventory: String,
    pub active: bool,
    pub trusted: bool,
    pub enabled: bool,
}

/// Portable full bundle detail for show/review/validate rendering (FEAT-020 D2).
///
/// Carries every semantic value the render helpers consume. The complete
/// `LoadedPlugin` never crosses the boundary; only branch-consumed fields are
/// projected here (D10).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginDetail {
    /// Inventory summary string (host-computed, e.g. `skills=1 mcp=0`).
    pub inventory_summary: String,
    pub name: String,
    pub id: String,
    pub version: String,
    pub origin: String,
    pub scope: String,
    pub state_label: String,
    pub trust_status: String,
    pub compatibility: String,
    pub content_hash: String,
    pub capability_hash: String,
    pub canonical_root: PathBuf,
    pub active: bool,
    pub trusted: bool,
    pub enabled: bool,
    pub unsupported_labels: Vec<String>,
    pub supported_labels: Vec<String>,
    pub skills: Vec<String>,
    pub filesystem_roots: Vec<String>,
    pub network_hosts: Vec<String>,
    pub stdio_mcp_servers: usize,
    pub lifecycle_mutation: bool,
    pub mcp_servers: Vec<PluginMcpServerDetail>,
    pub diagnostics: Vec<PluginDiagnostic>,
}

/// Portable outcome of a plugin mutation (FEAT-020 D2/D11).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginMutationOutcome {
    Installed,
    Updated,
    NoChange,
    Uninstalled,
    NeedsApproval(String),
    NetworkDenied(String),
}

/// Portable mutation receipt returned synchronously by the facet (FEAT-020 D11).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginMutationReceipt {
    pub name: String,
    pub path: Option<PathBuf>,
    pub content_hash: Option<String>,
    pub installed_content_hash: Option<String>,
    pub outcome: PluginMutationOutcome,
}

/// Portable bundle export receipt (FEAT-020 D2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginExportReceipt {
    pub exported_name: String,
    pub target: PathBuf,
    pub display_name: Option<String>,
    pub wrote_mcp_json: bool,
    pub files_copied: u64,
    pub skills_normalized: bool,
}

/// Portable legacy executable-tool detail (FEAT-020 D2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginLegacyTool {
    pub name: String,
    pub description: String,
    pub approval: String,
    pub input_schema: Option<String>,
    pub path: PathBuf,
}

/// Portable legacy-tool scan result: directory, discovered tools, and load
/// diagnostics for scripts that asked for something the loader ignored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginLegacyScan {
    pub dir: PathBuf,
    pub tools: Vec<PluginLegacyTool>,
    pub diagnostics: Vec<PluginDiagnostic>,
}

/// Portable Kimi managed-plugin candidate (FEAT-020 D2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginManagedCandidate {
    pub name: String,
    pub version: String,
    pub license: Option<String>,
    pub canonical_path: PathBuf,
    pub content_hash: String,
    pub capability_hash: String,
    pub inventory: String,
    pub applicable: bool,
}

/// Portable Kimi managed-scan result (FEAT-020 D2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginManagedScan {
    pub root: PathBuf,
    pub candidates: Vec<PluginManagedCandidate>,
    pub rejected: Vec<String>,
}

/// Portable review of a DeepSeek Harness bundle package before import: what
/// converts, what is skipped, the authority it will request, and the content
/// hash an exact install of the same package stages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginDshPreview {
    pub package_path: PathBuf,
    pub plugin_name: String,
    pub source_package: Option<String>,
    pub source_version: Option<String>,
    pub content_hash: String,
    pub skills: Vec<String>,
    pub remote_servers: Vec<String>,
    pub local_servers: Vec<String>,
    pub network_hosts: Vec<String>,
    /// Rows that become Native host code (JavaScript run by the extension host).
    pub native_rows: Vec<String>,
    pub requires_node: bool,
    /// One line per skipped row or patch operation (a manual port).
    pub manual_ports: Vec<String>,
    pub diagnostics: Vec<String>,
}

/// Portable marketplace candidate install plan (FEAT-020 D2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginMarketplaceInstallPlan {
    Supported { spec: String, source_kind: String },
    AlreadyPresent { selector: String, reason: String },
    Unsupported { reason: String },
}

/// Portable marketplace candidate (FEAT-020 D2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginMarketplaceCandidate {
    pub name: String,
    pub display_name: Option<String>,
    pub version: Option<String>,
    pub tier: String,
    pub compatibility: Option<String>,
    pub install_plan: PluginMarketplaceInstallPlan,
    pub description: Option<String>,
    pub homepage: Option<String>,
    pub repository: Option<String>,
    pub author: Option<String>,
    pub license: Option<String>,
    pub keywords: Vec<String>,
    pub when: Option<String>,
    pub diagnostics: Vec<PluginDiagnostic>,
    pub has_errors: bool,
}

/// Portable marketplace catalog (FEAT-020 D2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginMarketplaceCatalog {
    pub id: String,
    /// Source document path (for the `show` provenance line).
    pub source_path: Option<String>,
    pub display_name: Option<String>,
    pub description: Option<String>,
    pub format: String,
    pub tier: String,
    pub publisher: Option<String>,
    pub total_candidates: usize,
    pub warning_count: usize,
    pub candidates: Vec<PluginMarketplaceCandidate>,
    pub diagnostics: Vec<PluginDiagnostic>,
}

/// Portable marketplace add receipt (FEAT-020 D2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginMarketplaceAddReceipt {
    pub name: String,
    pub candidate_count: usize,
    pub warning_count: usize,
    pub catalog: PluginMarketplaceCatalog,
}

/// Portable marketplace state: stored catalogs plus an optional host-provided
/// built-in `official` catalog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginMarketplaceState {
    /// Optional host-provided built-in catalog. Current main provides none;
    /// retaining the option keeps the portable boundary future-compatible
    /// without inventing a catalog in the handler.
    pub official: Option<PluginMarketplaceCatalog>,
    pub stored: Vec<PluginMarketplaceCatalog>,
}

/// Portable suggestion for the `/plugin suggest` recommendation output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginSuggestion {
    pub name: String,
    /// State label rendered beside the plugin name (active/not-reviewed/…).
    pub state_label: String,
    pub description: String,
    pub why: Vec<String>,
    /// The actionable next step rendered under the suggestion.
    pub next_step: String,
}

/// Plugins hidden from proactive suggestions (plugin policy rule 9). Names
/// are lowercase; each list is sorted.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PluginSuggestionDismissals {
    /// "Don't suggest again": kept across sessions until reset.
    pub persisted: Vec<String>,
    /// Hidden for this session only (Esc, or a review the user already opened).
    pub session: Vec<String>,
}

/// Host plugin data for the plugin command group (FEAT-020 D1).
///
/// One object-safe, synchronous facet exposing the exact-minimum typed
/// operations the live `/plugin` branch closure consumes. Registry reads and
/// mutations, async-bridged install/update/uninstall (returning synchronous
/// portable receipts), export, legacy executable-tool scan, Kimi managed
/// import, and marketplace operations are all represented. The handler never
/// names `crate::plugins`, `PluginRegistry`, `LoadedPlugin`, `Config`, or
/// another concrete host service; implementation errors cross as safe text.
///
/// Post-mutation side effects (rediscovery, skill-cache refresh, active-skill
/// reset) happen host-side inside the facet implementation; the handler only
/// renders the returned receipt (D11).
pub trait CommandPluginContext {
    /// Read-only: registry summaries for list output.
    fn summaries(&self) -> Result<Vec<PluginSummary>, String>;
    /// Read-only: full portable detail for show/review/validate.
    fn detail(&self, selector: &str) -> Result<PluginDetail, String>;
    /// Read-only: registry-level diagnostics.
    fn registry_diagnostics(&self) -> Vec<PluginDiagnostic>;
    /// Read-only: whether validation reports no errors.
    fn validation_is_clean(&self) -> bool;
    /// Read-only: registry length (used by list/reload empty branches).
    fn len(&self) -> usize;
    /// Mutation: rediscover the workspace registry and refresh the skill
    /// cache; returns the new registry length for the reload message.
    fn reload(&mut self) -> Result<usize, String>;
    /// Read-only: whether the registry is empty.
    fn is_empty(&self) -> bool;
    /// Return the one-shot on-disk-change nudge, if the host detects one.
    /// The host owns the mutable catalog-stamp state; handlers only render.
    fn reload_nudge(&mut self) -> Option<String>;
    /// Read-only: persistence store path for marketplace state.
    fn state_path(&self) -> Option<PathBuf>;
    /// Read-only: recommend installed bundles for a task without side effects.
    fn suggest(&self, task: &str) -> Result<Vec<PluginSuggestion>, String>;
    /// Mutation: trust a bundle by exact review token. Success means the
    /// mutation was applied; the handler renders the action word from its own
    /// dispatch arm and may re-read `detail` for post-mutation state.
    fn trust(&mut self, selector: &str, token: &str) -> Result<(), String>;
    /// Mutation: enable a bundle. Success means enabled; re-read `detail` for
    /// the post-mutation compatibility note.
    fn enable(&mut self, selector: &str) -> Result<(), String>;
    /// Mutation: disable a bundle.
    fn disable(&mut self, selector: &str) -> Result<(), String>;
    /// Mutation: revoke trust.
    fn revoke_trust(&mut self, selector: &str) -> Result<(), String>;
    /// Async-bridged install; returns a synchronous portable receipt (D11).
    fn install(
        &mut self,
        source: &str,
        expected_content_hash: Option<&str>,
    ) -> Result<PluginMutationReceipt, String>;
    /// Async-bridged update; returns a synchronous portable receipt (D11).
    fn update(&mut self, selector: &str) -> Result<PluginMutationReceipt, String>;
    /// Async-bridged uninstall; returns a synchronous portable receipt (D11).
    fn uninstall(&mut self, selector: &str) -> Result<PluginMutationReceipt, String>;
    /// File-level removal of a just-installed bundle whose content hash
    /// mismatched (rollback). Unlike [`Self::uninstall`] it does not resolve a
    /// registry selector and triggers no rediscovery or skill-cache side
    /// effects; the host adapter owns the `crate::plugins` call (D1).
    fn uninstall_path(&mut self, name: &str, plugins_dir: &Path) -> Result<(), String>;
    /// Read-only: export a loaded bundle to a target directory.
    fn export(&self, selector: &str, target: &Path) -> Result<PluginExportReceipt, String>;
    /// Read-only: scan legacy executable plugin tools.
    fn legacy_scan(&self) -> Result<Option<PluginLegacyScan>, String>;
    /// Read-only: Kimi managed-plugin directory scan.
    fn managed_scan(&self, home_override: Option<&Path>) -> Result<PluginManagedScan, String>;
    /// Mutation: install a Kimi managed candidate by exact content hash.
    fn managed_install(
        &mut self,
        canonical_path: &Path,
        expected_content_hash: &str,
    ) -> Result<PluginMutationReceipt, String>;
    /// Read-only: convert a DeepSeek Harness bundle package into scratch and
    /// review it without installing anything.
    fn dsh_preview(&self, package: &Path) -> Result<PluginDshPreview, String> {
        let _ = package;
        Err("DeepSeek Harness import is not available in this host.".to_string())
    }
    /// Read-only: marketplace state (optional host catalog + stored catalogs).
    fn marketplace_state(&self) -> Result<PluginMarketplaceState, String>;
    /// Mutation: add a local catalog document to the marketplace store.
    fn marketplace_add(
        &mut self,
        name: &str,
        path: &Path,
    ) -> Result<PluginMarketplaceAddReceipt, String>;
    /// Mutation: remove a stored marketplace catalog.
    fn marketplace_remove(&mut self, name: &str) -> Result<bool, String>;
    /// Mutation: install a marketplace candidate through the reviewed installer.
    fn marketplace_install(
        &mut self,
        catalog: &str,
        candidate: &str,
    ) -> Result<PluginMutationReceipt, String>;
    /// Read-only: which plugins proactive suggestions currently skip.
    fn suggestion_dismissals(&self) -> Result<PluginSuggestionDismissals, String>;
    /// Mutation: let suggestions offer `name` again (every dismissed plugin
    /// when `None`), in this session and future ones. Returns the names
    /// cleared, sorted.
    fn reset_suggestion_dismissals(&mut self, name: Option<&str>) -> Result<Vec<String>, String>;
}

// ---------------------------------------------------------------------------
// Skill group (FEAT-022 D1)
// ---------------------------------------------------------------------------

/// Source provenance of a discovered skill (native file vs reviewed plugin snapshot).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillSourceKind {
    Native,
    Plugin {
        plugin_name: String,
        plugin_id: String,
    },
}

/// Curated product tier for bundled (shipped) skills.
///
/// The canonical name→tier classification stays in the TUI host
/// (`crate::skills::system::bundled_skill_tier`); the portable projection
/// carries the resolved tier so the handler can render the curated listing
/// without duplicating the canonical bundle list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillBundledTier {
    CoreAgentic,
    FormatTooling,
}

impl SkillBundledTier {
    /// Product-facing tier heading used by the `/skills` listing.
    #[must_use]
    pub fn heading(self) -> &'static str {
        match self {
            Self::CoreAgentic => "Core agentic",
            Self::FormatTooling => "Format & tooling",
        }
    }
}

/// One discovered skill entry (portable).
///
/// The body is intentionally excluded: activation and review receive body
/// text through their own delegates (`SkillActivationOutcome`/`ReviewOutcome`);
/// listing and inspect render name, description, source, and path only (D1
/// exact-minimum).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillEntry {
    pub name: String,
    pub description: String,
    pub source: SkillSourceKind,
    /// Native skills carry their on-disk path (inspect output).
    pub path: Option<String>,
    /// Bundled catalog tier; `None` for user/compatible skills.
    pub bundled_tier: Option<SkillBundledTier>,
}

/// Portable projection of the host skill registry (discovery, D1).
///
/// Carries every value the `/skills` and `/skill` handlers render: workspace
/// and configured skills dir displays, discovery mode label, searched
/// directories, entries, warnings, and the enabled-skill total.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillRegistryProjection {
    pub workspace: String,
    pub skills_dir: String,
    pub mode_label: String,
    pub dirs: Vec<String>,
    pub entries: Vec<SkillEntry>,
    pub warnings: Vec<String>,
    pub total: usize,
}

/// Target scope for skill mutations (`/skill install|update|uninstall|trust`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillTargetScope {
    Project,
    Global,
}

/// Portable mutation outcome mirroring the host receipt variants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillMutationOutcome {
    Installed,
    Updated,
    NoChange,
    Removed,
    Trusted,
    Imported,
    AlreadyPresent,
    NeedsApproval(String),
    NetworkDenied(String),
}

/// Synchronous portable receipt for a skill mutation (FEAT-020 D11 mirror):
/// the host owns the async network bridge; the handler renders the receipt
/// byte-identically from these values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillMutationReceipt {
    pub name: String,
    pub safe_target_path: String,
    pub outcome: SkillMutationOutcome,
}

/// One curated remote registry entry (`/skills --remote`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteSkillEntry {
    pub name: String,
    pub description: Option<String>,
    pub source: String,
}

/// Remote registry fetch outcome (`/skills --remote`, suggest source).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteRegistryOutcome {
    Loaded { entries: Vec<RemoteSkillEntry> },
    NeedsApproval(String),
    Denied(String),
}

/// Remote recommendation for `/skills suggest <task>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillRecommendation {
    pub name: String,
    pub description: Option<String>,
    pub matched_terms: Vec<String>,
}

/// Per-skill outcome of `/skills sync`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillSyncEntry {
    Downloaded { name: String, path: String },
    Fresh { name: String },
    Failed { name: String, reason: String },
    Denied { name: String, host: String },
    NeedsApproval { name: String, host: String },
}

/// Aggregate `/skills sync` outcome.
///
/// Registry-level network-policy outcomes are carried as variants so the
/// portable handler composes the exact `needs_approval` / `denied` messages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillSyncOutcome {
    Done {
        total: usize,
        downloaded: usize,
        fresh: usize,
        failed: usize,
        entries: Vec<SkillSyncEntry>,
    },
    RegistryNeedsApproval(String),
    RegistryDenied(String),
}

/// Successful skill activation data (host performs the side effects).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillActivationOutcome {
    pub name: String,
    pub description: String,
}

/// Activation failures with the exact data the handler renders.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillActivationError {
    NotFound {
        requested: String,
        available: Vec<String>,
        warnings: Vec<String>,
    },
    InvocationRejected {
        name: String,
        reason: String,
    },
    PluginRejected {
        name: String,
        reason: String,
    },
}

/// `/review` outcome data (host performs the side effects).
///
/// On success the baseline `/review` renders no message — it only emits the
/// `SendMessage` action — so `Ready` carries no payload (D1 exact-minimum).
/// Warnings are only rendered on the not-found path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReviewOutcome {
    Ready,
    NotFound {
        skills_dir: String,
        global_dir: String,
        warnings: Vec<String>,
    },
}

/// One snapshot entry for `/restore` listings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotEntry {
    pub id: String,
    pub label: String,
    pub timestamp: i64,
}

/// Host approval posture for the `/restore` trust gate (D4: no MODE_POLICY).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandApprovalState {
    pub yolo: bool,
    pub trust_mode: bool,
}

/// Host skill data for the skills command group (FEAT-022 D1).
///
/// Exposes the typed, exact-minimum operations the live skills handlers
/// consume: discovery (`/skills`), activation (`/skill`), synchronous
/// mutation receipts (`/skill install|update|uninstall|trust`), remote
/// registry + sync (`/skills --remote|sync|suggest`), review (`/review`),
/// and snapshot list/restore plus approval state (`/restore`). The host
/// adapter is the only place that touches `App`, `crate::plugins`,
/// `SnapshotRepo`, `crate::skills` services, config/network policy, and the
/// async runtime bridge. The shared FEAT-015 `CommandSkillsContext` is never
/// widened; active-skill reads use that facet, mutations flow through the
/// delegates here (D2). All results are contract-owned portable values;
/// implementation errors cross as safe text. `/skill` declares this facet
/// plus `CommandSkillsContext` for the baseline cache-refresh policy;
/// `/skills`, `/review`, and `/restore` declare exactly this facet.
pub trait CommandSkillGroupContext {
    /// `/skills` discovery projection (workspace, skills dir, scan mode,
    /// searched directories, plugin-provided skills, warnings).
    fn skill_registry_projection(&self) -> SkillRegistryProjection;
    /// `/skill` activation: host lookup, plugin-authority verification, and
    /// active-skill/history side effects. `SendMessage` task composition is
    /// handler-side.
    fn activate_skill(
        &mut self,
        name: &str,
    ) -> Result<SkillActivationOutcome, SkillActivationError>;
    /// `/skill install` — synchronous portable receipt; host owns network/async.
    fn install_skill(
        &mut self,
        scope: Option<SkillTargetScope>,
        spec: &str,
    ) -> Result<SkillMutationReceipt, String>;
    /// `/skill update` — synchronous portable receipt; host owns network/async.
    fn update_skill(
        &mut self,
        scope: Option<SkillTargetScope>,
        name: &str,
    ) -> Result<SkillMutationReceipt, String>;
    /// `/skill uninstall` — synchronous portable receipt.
    fn uninstall_skill(
        &mut self,
        scope: Option<SkillTargetScope>,
        name: &str,
    ) -> Result<SkillMutationReceipt, String>;
    /// `/skill trust` — synchronous portable receipt.
    fn trust_skill(
        &mut self,
        scope: Option<SkillTargetScope>,
        name: &str,
    ) -> Result<SkillMutationReceipt, String>;
    /// `/skills --remote` registry fetch (network policy host-side).
    fn fetch_remote_registry(&mut self) -> Result<RemoteRegistryOutcome, String>;
    /// `/skills suggest <task>` — host fetch + recommendation computation.
    fn recommend_skills(&mut self, task: &str) -> Result<Vec<SkillRecommendation>, String>;
    /// `/skills sync` — host registry sync (async bridge host-side).
    fn sync_registry(&mut self) -> Result<SkillSyncOutcome, String>;
    /// `/review` activation: host discovery + side effects (empty-target
    /// validation and `SendMessage` composition are handler-side).
    fn run_review(&mut self) -> Result<ReviewOutcome, String>;
    /// `/restore` snapshot listing.
    fn snapshot_list(&mut self, limit: usize) -> Result<Vec<SnapshotEntry>, String>;
    /// `/restore <N>`: host restores by snapshot id; handler composes the
    /// exact success message from its list entry.
    fn restore_snapshot(&mut self, id: &str) -> Result<(), String>;
    /// `/restore` trust gate posture (yolo / trust_mode).
    fn approval_state(&self) -> CommandApprovalState;
}

// ---------------------------------------------------------------------------
// Session lifecycle capability (FEAT-023).
//
// One contract-owned facet for the seven host-dependent lifecycle commands;
// `/compact` and `/purge` stay pure. The shared `CommandSessionContext` above
// stays unchanged: it
// serves commands outside this slice and must not gain persistence,
// navigation, picker, or lifecycle mutation authority (D2). No concrete App,
// SessionManager, session-journal, picker, configuration, or view-stack type
// crosses this boundary; successful results are structured portable fields so
// the handlers retain exact message composition (D2/D5).
// ---------------------------------------------------------------------------

/// Portable synchronization fields carried by shared session and debug actions.
/// Conversation and prompt types are protocol-owned shapes; concrete runtime
/// state and host action conversion remain outside the command contract.
#[derive(Clone, Debug, PartialEq)]
pub struct SessionSyncPayload {
    pub session_id: Option<String>,
    pub messages: Vec<Message>,
    pub system_prompt: Option<SystemPrompt>,
    pub model: String,
    pub workspace: PathBuf,
    pub mode: CommandMode,
}

/// `/branch` success projection (`session/branch.rs`). The handler composes
/// the exact success line from these deterministic fields.
#[derive(Clone, Debug, PartialEq)]
pub struct SessionBranchOutcome {
    pub leaf_display: String,
    pub journal_entries_before: usize,
    pub sync: SessionSyncPayload,
}

/// `/fork` success projection for an active-conversation fork. The handler
/// composes `Forked session {parent} -> {fork}` from these required fields.
#[derive(Clone, Debug, PartialEq)]
pub struct SessionForkReceipt {
    pub parent_label: String,
    pub fork_label: String,
    pub sync: SessionSyncPayload,
}

/// `/fork <session_id|prefix>` success projection. Explicit-source forks
/// always report their spawn depth, so the contract makes that field required
/// rather than permitting an invalid missing-depth state.
#[derive(Clone, Debug, PartialEq)]
pub struct SessionForkFromReceipt {
    pub parent_label: String,
    pub fork_label: String,
    pub spawn_depth: u64,
    pub sync: SessionSyncPayload,
}

/// `/save` success projection. The host performs the full baseline sequence
/// (snapshot, serialization, atomic write, metadata application, work-state
/// publication); the handler renders `Session saved to {display_path} (ID:
/// {truncated_id})`.
#[derive(Clone, Debug, PartialEq)]
pub struct SessionSaveReceipt {
    pub display_path: String,
    pub truncated_id: String,
}

/// `/new` success projection. The handler renders
/// `Started new session {truncated_id} (New Session). Previous sessions
/// remain available via /resume.`
#[derive(Clone, Debug, PartialEq)]
pub struct SessionNewReceipt {
    pub truncated_id: String,
    pub sync: SessionSyncPayload,
}

/// `/sessions archive|unarchive|restore` success projection. The handler
/// renders `Archived session {id} ({title})` or `Restored session ...` from
/// the verb it dispatched.
#[derive(Clone, Debug, PartialEq)]
pub struct SessionArchiveReceipt {
    pub truncated_id: String,
    pub title: String,
}

/// `/tree` body projection. The body rendering source (journal tree and
/// linear transcript) stays TUI-owned; the handler appends the exact
/// guidance lines (D5).
#[derive(Clone, Debug, PartialEq)]
pub enum TreeBodyProjection {
    /// Journal render already includes the trailing newline before guidance.
    Journal {
        rendered: String,
    },
    /// Linear pre-journal render (the marker lines).
    Linear {
        rendered: String,
    },
    EmptySession,
    NoSession,
}

/// Lifecycle authority for the session command slice (FEAT-023 D2).
///
/// Operation-granular synchronous delegates over the exact minimum host work
/// the nine commands consume. Delegates may return the explicit host-error
/// text the baseline surfaces for a failing stage; successful results are
/// structured portable fields so handlers retain byte-identical composition.
pub trait CommandSessionLifecycleContext {
    /// Live transition gate. Handlers return their own blocked-error text
    /// before invoking any mutating delegate, matching the baseline ordering
    /// (`/branch`, `/fork`, `/load`, `/new`). `/fork picker` and `/tree`
    /// never consult it in the baseline, so their paths must not either.
    fn transition_blocked(&self) -> bool;

    /// Why the transition gate is closed, in operator terms; empty means the
    /// transition is allowed. Consulted only after [`Self::transition_blocked`]
    /// reports a block, so the refusal can name the work that holds the
    /// session instead of restating that it is held.
    ///
    /// Display projection, not a data source: the strings are pre-rendered
    /// for a refusal message and carry no stable shape. A caller that needs
    /// ids, durations, or owners should add a structured method instead of
    /// parsing these back out.
    fn transition_blockers(&self) -> Vec<String>;

    /// `/branch` with no argument: the current leaf when an active journaled
    /// session resolves, otherwise `None` (the baseline silently falls back
    /// to the usage message on this path).
    fn branch_current_leaf_hint(&self) -> Option<String>;

    /// `/branch <entry_id>`: persist the leaf move and apply the branched
    /// transcript. Errors are the exact baseline message for the failing
    /// stage (no active session, directory open, load, persist, or branch
    /// failure).
    fn branch_to(&mut self, entry_id: &str) -> Result<SessionBranchOutcome, String>;

    /// `/tree`: produce the journal/linear/empty/no-session projection.
    /// Errors are the exact baseline directory-open message.
    fn tree_body(&self) -> Result<TreeBodyProjection, String>;

    /// `/save [path]`: the full baseline persistence sequence.
    fn save_session(&mut self, explicit_path: Option<String>)
    -> Result<SessionSaveReceipt, String>;

    /// `/fork` (active conversation): the full baseline parent/child save and
    /// switch sequence.
    fn fork_active(&mut self) -> Result<SessionForkReceipt, String>;

    /// `/fork <session_id|prefix>`: explicit-source fork.
    fn fork_from(&mut self, session_id_or_prefix: &str) -> Result<SessionForkFromReceipt, String>;

    /// `/new [--force]`: fresh-session transition. The caller has already
    /// parsed the argument and applied the transition-blocked gate; blocker,
    /// busy-work-state, and success handling match the baseline.
    fn fresh_session(&mut self, force: bool) -> Result<SessionNewReceipt, String>;

    /// `/load <path>`: resolve the path (separator-bearing direct vs
    /// workspace-relative) and validate the saved-session shape without
    /// applying state or emitting a premature success receipt.
    fn load_session(&mut self, path: &str) -> Result<PathBuf, String>;

    /// `/sessions` picker open with optional preselection (bare, `show`,
    /// `list`, `picker`, and `open <id>` forms). Picker construction and
    /// locale selection stay host-side.
    fn open_picker(&mut self, preselected: Option<String>);

    /// `/sessions archive|unarchive|restore <id>`: durable lifecycle state
    /// update that also syncs the live cached metadata atomically.
    fn set_archived(
        &mut self,
        session_id: &str,
        archived: bool,
    ) -> Result<SessionArchiveReceipt, String>;

    /// `/sessions prune <days>`: prune persisted sessions older than `days`
    /// days while protecting the active session; returns the number pruned.
    fn prune_sessions(&mut self, days: u64) -> Result<usize, String>;
}

// ---------------------------------------------------------------------------
// FEAT-024: session control slice (D2-D7).
//
// One independently optional session-control authority covering exactly the
// host work the six control commands (`/relay`, `/rename`, `/resume`, `/rc`,
// `/remote-env`, `/title`) consume. `CommandSessionContext` and
// `CommandSessionLifecycleContext` are deliberately not widened: control
// authority exists only on this facet, and every delegate is an atomic host
// operation or a semantic projection so handlers keep byte-identical
// composition. Portable values never expose TUI state beyond what the
// baseline branches on.
// ---------------------------------------------------------------------------

/// `/relay` semantic snapshot (D4). The handler composes the byte-identical
/// instruction from these deterministic fields; `crate::prompts`,
/// `crate::todo_snapshot`, goal/todo/plan machinery, Work-state objects, and
/// locks stay host-side.
#[derive(Clone, Debug, PartialEq)]
pub struct RelayProjection {
    /// Authoritative compact-template text (`COMPACT_TEMPLATE`), echoed with
    /// a trailing trim by the handler exactly as today.
    pub compact_template: String,
    /// Workspace-relative path the relay is written to. The host supplies
    /// the same path its startup loader reads first, so the writer and the
    /// loader cannot drift.
    pub handoff_path: String,
    pub workspace: String,
    pub mode: String,
    pub model: String,
    pub goal_objective: Option<String>,
    pub goal_token_budget: Option<u32>,
    pub todos: TodoProjection,
    pub plan: PlanProjection,
}

/// To-do state distinction for the relay snapshot. The rendered body (if any)
/// is produced host-side from the authoritative graph-backed snapshot seam.
#[derive(Clone, Debug, PartialEq)]
pub enum TodoProjection {
    /// Rendered to-do body lines.
    Body(String),
    /// Work state could not be read (`To-do: unavailable because the list is
    /// busy.`).
    Unavailable,
    /// No Work state or no to-do body.
    Absent,
}

/// Plan-state distinction for the relay snapshot. `Busy` reproduces the
/// baseline `try_lock` failure branch; `Absent` reproduces an empty snapshot.
///
/// `PlanSections` is intentionally not boxed: the command-crate boundary gate
/// forbids boxed storage in the contract, and the section payload is only ever
/// built once per `/relay` dispatch.
#[derive(Clone, Debug, PartialEq)]
#[allow(clippy::large_enum_variant)]
pub enum PlanProjection {
    Sections(PlanSections),
    Busy,
    Absent,
}

/// Semantic plan snapshot fields consumed by `/relay`. Values are the raw
/// snapshot values; the handler applies the baseline trim/empty filtering and
/// label composition so ordering and spacing stay byte-identical.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PlanSections {
    pub title: Option<String>,
    pub objective: Option<String>,
    pub context_summary: Option<String>,
    pub explanation: Option<String>,
    pub sources_used: Vec<String>,
    pub critical_files: Vec<String>,
    pub constraints: Vec<String>,
    pub recommended_approach: Option<String>,
    pub verification_plan: Option<String>,
    pub risks_and_unknowns: Option<String>,
    pub handoff_packet: Option<String>,
    pub items: Vec<PlanStep>,
}

/// Portable plan-step status. The adapter maps the TUI plan status onto this
/// semantic enum; the command handler remains the sole owner of the exact
/// `pending`/`in_progress`/`completed` labels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlanStepStatus {
    Pending,
    InProgress,
    Completed,
}

/// One semantic plan checklist item.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlanStep {
    pub status: PlanStepStatus,
    pub text: String,
}

/// `/resume` route resolution (D6). The host resolves argument shape and
/// performs container imports atomically; the handler selects the exact
/// baseline message/action per variant.
#[derive(Clone, Debug, PartialEq)]
pub enum ResumeSource {
    /// Argument resolves to a readable file (`raw` direct path or
    /// workspace-relative path); the handler calls `import_session_file`.
    File(PathBuf),
    /// Argument resolved through the session manager (id or prefix).
    Session {
        /// Durable session file when present; `None` reproduces the baseline
        /// non-file fallback message arm.
        load_path: Option<PathBuf>,
        truncated_id: String,
        title: String,
    },
    /// Argument parsed as a foreign session container, which was imported
    /// atomically by the resolver.
    Imported(ResumeImportReceipt),
    /// Argument matched neither a file, a session, nor a container.
    NotFound { raw: String, error: String },
}

/// Portable `/resume` import receipt. The handler renders
/// `Imported foreign session as {truncated_id} ({entry_count} entries, leaf
/// {leaf_display})`, and carries `sync` so the engine adopts the imported
/// conversation instead of staying on the previous one.
#[derive(Clone, Debug, PartialEq)]
pub struct ResumeImportReceipt {
    pub truncated_id: String,
    pub entry_count: usize,
    pub leaf_display: String,
    pub sync: SessionSyncPayload,
}

/// `/rename` success receipt; the title is the sanitized persisted value so
/// the handler echoes exactly what was written.
#[derive(Clone, Debug, PartialEq)]
pub struct SessionTitleReceipt {
    pub title: String,
}

/// Bare `/title` status projection (`Window title: [{effective}]{source}`).
#[derive(Clone, Debug, PartialEq)]
pub struct TitleReport {
    /// Effective window-title prefix, or `unset`.
    pub effective: String,
    pub source: TitleSource,
}

#[derive(Clone, Debug, PartialEq)]
pub enum TitleSource {
    /// Session-level window title set.
    Session,
    /// Config-default title applies.
    ConfigDefault,
    /// Neither a session title nor a config default.
    None,
}

/// `/rc link` structured link data.
#[derive(Clone, Debug, PartialEq)]
pub struct RemoteLink {
    pub url: String,
    pub computer_url: Option<String>,
}

/// `/rc open` outcome. Browser launch stays synchronous and single-attempt;
/// no deferred external-URL action is produced (D6).
#[derive(Clone, Debug, PartialEq)]
pub enum RemoteOpenOutcome {
    NoLink,
    Opened { url: String },
    LaunchFailed { url: String },
}

/// `/rc start` wording input: the active-turn copy is used while a turn is
/// loading or a dispatch is in flight.
#[derive(Clone, Debug, PartialEq)]
pub struct RemoteStartInfo {
    pub connecting: bool,
}

/// `/remote-env open` hosted-work target. The URL is fully encoded host-side
/// (the portable handler must not depend on `urlencoding`); repo/branch echo
/// the raw values used by the baseline message replacements.
#[derive(Clone, Debug, PartialEq)]
pub struct HostedWorkTarget {
    pub url: String,
    pub repo: String,
    pub branch: String,
}

/// Control authority for the session command slice (FEAT-024 D2/D5).
///
/// Operation-granular synchronous delegates over the exact minimum host work
/// the six control commands consume. Delegates reproduce the baseline
/// check/mutation order (transition gate before resume I/O, save before
/// publication, single-attempt browser launch) and return portable receipts/
/// projections or the exact host-error text the baseline surfaces. No
/// `SessionManager`, saved-session/container type, `SessionPickerView`,
/// remote-control service, Git wrapper, configuration, model/history type,
/// lock, or host callback crosses the facet.
pub trait CommandSessionControlContext {
    /// Live transition gate consulted by `/resume` before any picker or I/O.
    fn transition_blocked(&self) -> bool;

    /// Why the transition gate is closed, in operator terms; empty means the
    /// transition is allowed. Consulted only after [`Self::transition_blocked`]
    /// reports a block.
    ///
    /// Display projection, not a data source: the strings are pre-rendered
    /// for a refusal message and carry no stable shape. A caller that needs
    /// ids, durations, or owners should add a structured method instead of
    /// parsing these back out.
    fn transition_blockers(&self) -> Vec<String>;

    /// `/relay`: authoritative semantic snapshot (workspace/mode/model/goal/
    /// to-do/plan/compact-template). Unavailable sources are represented as
    /// explicit states, never panics.
    fn relay_projection(&self) -> RelayProjection;

    /// Bare `/resume`: push the existing picker without preselection.
    fn open_resume_picker(&mut self);

    /// `/resume <raw>`: resolve direct-path, workspace-relative, session
    /// id/prefix, and inline-container routes in the established order; a
    /// recognized inline container is imported atomically here.
    fn resolve_resume_source(&mut self, raw: &str) -> Result<ResumeSource, String>;

    /// `/resume <file>`: read, parse, persist, and apply a foreign session
    /// file (container or plain saved session). Errors are the exact baseline
    /// read/parse/import text.
    fn import_session_file(&mut self, path: PathBuf) -> Result<ResumeImportReceipt, String>;

    /// Apply the authoritative session-title character policy before the
    /// portable `/rename` and `/title` handlers validate and compose output.
    fn sanitize_session_title(&self, raw_title: &str) -> String;

    /// `/rename <title>`: recover first-snapshot state, sync live state,
    /// persist, and publish with baseline order. The handler already applied
    /// sanitization plus blank and 100-character validation.
    fn rename_session(&mut self, title: &str) -> Result<SessionTitleReceipt, String>;

    /// Bare `/title`: effective prefix and its source.
    fn title_report(&self) -> TitleReport;

    /// `/title <title>`: persist an already sanitized and validated window
    /// title with baseline save/publication/redraw semantics.
    fn set_window_title(&mut self, title: String) -> Result<(), String>;

    /// `/title off|clear|none`: clear the session window title with the same
    /// baseline persistence/redraw semantics.
    fn clear_window_title(&mut self) -> Result<(), String>;

    /// `/rc status`: current remote-control status line.
    fn remote_status(&self) -> String;

    /// `/rc link`: live session link plus optional computer-management URL.
    fn remote_link(&self) -> Option<RemoteLink>;

    /// `/rc open`: synchronous single browser attempt over the authoritative
    /// URL-opening helper; outcome carries the URL for exact message text.
    fn remote_browser_open(&self) -> RemoteOpenOutcome;

    /// `/rc start`: whether the active-turn copy applies.
    fn remote_start_info(&self) -> RemoteStartInfo;

    /// `/rc stop`: refusal reason while a remote turn/envelope is active.
    fn remote_stop_refusal(&self) -> Option<String>;

    /// `/remote-env open`: validate the hosted-work Git target host-side and
    /// return the encoded URL plus raw repo/branch echoes; `None` reproduces
    /// the unavailable-target error. Credentials never appear in values or
    /// errors.
    fn resolve_hosted_work_target(&self) -> Option<HostedWorkTarget>;
}

// ---------------------------------------------------------------------------
// FEAT-025: session export slice (D1-D9).
//
// One independently optional session-export authority covering exactly the
// host work `/export` (and its `/daochu` alias) consumes. The shared
// `CommandSessionContext`, `CommandSessionLifecycleContext`, and
// `CommandSessionControlContext` facets are deliberately not widened: export
// authority exists only on this facet, and every delegate is an atomic host
// operation or a semantic projection so the portable handler keeps
// byte-identical composition. Hidden payloads are excluded while projections
// are built (D9), so internal reasoning, reasoning signatures, and inline or
// local image bytes never enter these DTOs. No `App`, clipboard handler,
// snapshot repository, history cell, session manager, configuration, client,
// filesystem handle, or host callback crosses this boundary (D1/D3/D5/D7).
// ---------------------------------------------------------------------------

/// Portable conversation metadata for the export header (D3).
///
/// Values that already have an authoritative host derivation keep it
/// (session-label truncation, provider identity, model label, mode display,
/// workspace basename, message count, clock); portable rendering adds only
/// export formatting and sanitization (D10).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExportMetadata {
    /// Host-truncated session id, or the baseline `unsaved` fallback.
    pub session_label: String,
    pub provider: String,
    pub model: String,
    pub mode: String,
    /// Workspace directory basename, or the baseline `workspace` fallback.
    pub workspace_name: String,
    /// `api_messages.len()` when authoritative, otherwise `history.len()`.
    pub message_count: usize,
    pub exported_at_unix: i64,
}

/// One tool-call caller projection (D3). Only the fields the baseline export
/// renders cross the boundary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolCallerProjection {
    pub caller_type: String,
    pub tool_id: Option<String>,
}

/// One projected content block (D3/D9).
///
/// Visible text and structured content cross as portable data; internal
/// reasoning bodies, reasoning signatures, and inline or local image payloads
/// are replaced by typed omission markers at projection time and never cross.
#[derive(Clone, Debug, PartialEq)]
pub enum ExportBlock {
    /// Visible text block; portable rendering sanitizes it.
    Text {
        text: String,
    },
    /// External image reference (`http`/`https` only); portable rendering
    /// redacts credential-bearing URLs.
    ImageReference {
        url: String,
    },
    /// Inline or local image payload excluded at projection time (D9).
    ImageOmitted,
    /// Internal reasoning body and reasoning signature excluded (D9).
    InternalReasoning,
    ToolCall {
        id: String,
        name: String,
        caller: Option<ToolCallerProjection>,
        input: Value,
    },
    ToolResult {
        tool_use_id: String,
        content: String,
        is_error: bool,
        /// `Some` when the host message carried structured result blocks; the
        /// host has already applied the safe-result filter (D9).
        structured: Option<Value>,
    },
    ServerToolCall {
        id: String,
        name: String,
        input: Value,
    },
    ToolSearchResult {
        tool_use_id: String,
        content: Value,
    },
    CodeExecutionResult {
        tool_use_id: String,
        content: Value,
    },
}

/// One projected authoritative message (D3).
///
/// `prompt_snippet` is the host-computed `snapshot_label_prompt_snippet` of
/// the first visible text block. The parser and snippet algorithm stay
/// TUI-owned (D8), so correlation compares authoritative values instead of
/// re-deriving them portably.
///
/// `is_user_role` carries the host's exact `Role::User` comparison. `role` is
/// the rendered wire string, and comparing it textually would also match a
/// `Role::Unrecognized("user")`, which the baseline never treated as a user
/// turn. The flag keeps restore-point correlation faithful to the baseline.
#[derive(Clone, Debug, PartialEq)]
pub struct ExportMessage {
    pub role: String,
    /// Exact `message.role == Role::User`, not a string comparison.
    pub is_user_role: bool,
    pub blocks: Vec<ExportBlock>,
    pub prompt_snippet: Option<String>,
}

/// One projected visible-history fallback entry (D3).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HistoryEntry {
    /// Visible host content that portable rendering must still sanitize.
    Sanitized { role: String, body: String },
    /// An already-final baseline marker line that must not be sanitized again.
    Literal { role: String, body: String },
}

/// Transcript source precedence (D3): authoritative API messages when
/// present, otherwise the sanitized visible-history fallback.
#[derive(Clone, Debug, PartialEq)]
pub enum TranscriptProjection {
    Authoritative(Vec<ExportMessage>),
    HistoryFallback(Vec<HistoryEntry>),
}

/// One snapshot projected to semantic fields (D8).
///
/// `kind`, `sequence`, and `prompt_snippet` are the host-parsed label fields;
/// the raw `label` is kept only for the human-readable table column. No
/// preformatted correlation line crosses the boundary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RestoreSnapshot {
    pub id: String,
    pub label: String,
    pub timestamp_unix: i64,
    pub kind: String,
    pub sequence: Option<u64>,
    pub prompt_snippet: Option<String>,
}

/// Restore-point projection with distinct baseline states (D3/D8).
///
/// `None` means no snapshot repository exists, `Unreadable` preserves the host
/// failure reason, and `Recorded` distinguishes an existing-but-empty
/// repository from one with snapshots by the vector length.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RestorePointProjection {
    None,
    Unreadable { reason: String },
    Recorded { snapshots: Vec<RestoreSnapshot> },
}

/// Full conversation projection (D3/D8/D9).
#[derive(Clone, Debug, PartialEq)]
pub struct ConversationExportProjection {
    pub metadata: ExportMetadata,
    pub transcript: TranscriptProjection,
    pub restore_points: RestorePointProjection,
}

/// Turn-handoff projection (D2).
///
/// `markdown` is the unmodified shared TUI renderer output and
/// `workspace_path` is the value the portable handler replaces with `.` after
/// sanitizing; the renderer itself is neither moved nor duplicated.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TurnHandoffProjection {
    pub markdown: String,
    pub workspace_path: String,
}

/// Session-export authority for the `/export` slice (FEAT-025 D1-D9).
///
/// Operation-granular synchronous delegates over the exact minimum host work
/// the command consumes. The portable handler parses the request first, renders
/// the selected scope second, and then uses these delegates in baseline order:
/// clipboard exports call terminal-paste detection, recovery write, and
/// clipboard delivery exactly once each with the same Markdown; file exports
/// resolve the destination before writing it. A recovery-write `None` never
/// prevents the clipboard attempt, and a turn-only export never requests the
/// conversation projection (D6/D7).
pub trait CommandSessionExportContext {
    /// Conversation export projection: metadata, authoritative-or-fallback
    /// transcript, and restore-point state. Read-only; opens only an existing
    /// snapshot repository and never creates one (D8).
    fn conversation_projection(&self) -> ConversationExportProjection;

    /// Turn-handoff projection: unmodified shared renderer Markdown plus the
    /// workspace path value (D2).
    fn turn_handoff_projection(&self) -> TurnHandoffProjection;

    /// Whether clipboard delivery goes through the terminal-client (SSH/OSC 52
    /// via tmux) path (D6).
    fn clipboard_requires_terminal_paste(&self) -> bool;

    /// Write the shared `last-copy.md` recovery file. `None` reproduces the
    /// baseline silent failure; recovery writing never falls through to an
    /// error (D5/D6).
    fn write_recovery_copy(&self, markdown: &str) -> Option<PathBuf>;

    /// Attempt clipboard delivery. `Err` carries the raw host clipboard error
    /// text; the handler composes the exact failure wording (D6).
    fn write_clipboard(&self, markdown: &str) -> Result<(), String>;

    /// Resolve a file destination exactly as the baseline does (trim, empty
    /// check, `..` rejection, workspace canonicalization and rebasing, filename
    /// requirement). Errors are returned unwrapped (D7).
    fn resolve_export_path(&self, raw: &str) -> Result<PathBuf, String>;

    /// Write the rendered export to a resolved destination with the baseline
    /// protection checks. Errors are returned unwrapped; the handler wraps them
    /// in `Failed to export {label} to {path}: {err}` (D7).
    fn write_export_file(&self, path: &Path, contents: &[u8], force: bool) -> Result<(), String>;
}

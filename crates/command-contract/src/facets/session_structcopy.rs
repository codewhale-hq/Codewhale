//! Read-only observations for one human-selected structural copy.
//! Replaces the command's concrete session/model/plan observations. The host
//! excludes private instruction/reasoning and inline image bodies before this
//! boundary; portable rendering still sanitizes visible content. These are not
//! complete session objects, executable callbacks, or pre-rendered envelopes.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StructcopyError {
    Unavailable,
    Busy,
    /// Raw preparation error text already exposed by the original command.
    Preparation(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct StructcopyTranscript {
    pub index: usize,
    pub role: String,
    pub content: StructcopyContent,
}

#[derive(Debug, Clone, PartialEq)]
pub enum StructcopyContent {
    InternalContext,
    Visible(Vec<StructcopyBlock>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum StructcopyBlock {
    Text(String),
    ThinkingOmitted,
    ToolUse {
        id: String,
        name: String,
        input: Value,
        caller_type: Option<String>,
    },
    ToolResult {
        tool_use_id: String,
        result: StructcopyToolResult,
    },
    /// Only HTTP(S) URLs; the host must omit local and inline image payloads.
    ImageUrl(String),
    ImageOmitted,
    ServerToolUse {
        id: String,
        name: String,
        input: Value,
    },
    ToolSearchToolResult {
        tool_use_id: String,
        content: Value,
    },
    CodeExecutionToolResult {
        tool_use_id: String,
        content: Value,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct StructcopyToolPair {
    pub name: String,
    pub input: Value,
    /// No result is distinct from a result with unknown error status.
    pub result: Option<StructcopyToolResult>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StructcopyToolResult {
    pub content: String,
    pub is_error: Option<bool>,
    /// The host's existing safe structured-result filter has already run.
    pub content_blocks: Option<Vec<Value>>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StructcopyPlan {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub objective: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub explanation: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources_used: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub critical_files: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub constraints: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recommended_approach: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification_plan: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub risks_and_unknowns: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handoff_packet: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub replan_if: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub items: Vec<StructcopyPlanItem>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StructcopyPlanItem {
    pub step: String,
    pub status: StructcopyPlanStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StructcopyPlanStatus {
    Pending,
    InProgress,
    Completed,
}

/// Typed form of the existing bounded workflow summary projection. It carries
/// no raw journal events, owner IDs, paths, or executable runtime state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StructcopyWorkflow {
    pub run_id: String,
    pub status: StructcopyWorkflowStatus,
    pub lifecycle_seq: u64,
    pub started_at_ms: u64,
    pub completed_at_ms: Option<u64>,
    pub source_file: Option<String>,
    pub workflow_id: Option<String>,
    pub workflow_goal: Option<String>,
    pub token_budget: Option<u64>,
    pub child_count: usize,
    pub schema_error_count: usize,
    pub schema_repair_count: u64,
    pub dispatch_failure_count: u64,
    pub progress_count: u64,
    pub last_progress: Option<String>,
    pub event_count: usize,
    pub last_event_type: Option<String>,
    /// Unknown execution counts serialize as null, never measured zero.
    pub leaf_count: Option<usize>,
    pub branch_count: Option<usize>,
    pub control_count: Option<usize>,
    pub execution_status: Option<StructcopyExecutionStatus>,
    pub gate_count: usize,
    pub blocked_gate_count: usize,
    pub gate_status: Vec<StructcopyGate>,
    pub error: Option<String>,
    pub usage: Option<StructcopyUsage>,
    pub events_dropped: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StructcopyWorkflowStatus {
    Running,
    Completed,
    Degraded,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StructcopyExecutionStatus {
    Pending,
    Running,
    Succeeded,
    Failed,
    Cancelled,
    BudgetExceeded,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StructcopyGate {
    pub gate_id: String,
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_reason: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StructcopyUsage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_microusd: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<u64>,
    #[serde(default)]
    pub tasks_reported: u64,
}

/// Host observations only. Root eligibility, sorting and labels are command
/// policy, and remain portable. Canonicalization failure is ordinary absence.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StructcopyPathRoots {
    pub workspace: String,
    pub canonical_workspace: Option<String>,
    pub home: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StructcopyTransport {
    Native,
    /// Terminal-client output is queued, never delivery confirmation.
    TerminalQueued,
}

/// Exact authority for `/structcopy`, independent of session export/recovery.
/// Observations do not mutate the session. Clipboard is the sole side effect;
/// the caller chooses it only after successful rendering and never for stdout.
pub trait CommandSessionStructcopyContext {
    fn transcript_item(&self, index: usize) -> Result<StructcopyTranscript, StructcopyError>;
    fn tool_pair(&self, call_id: &str) -> Result<StructcopyToolPair, StructcopyError>;
    fn plan_snapshot(&self) -> Result<StructcopyPlan, StructcopyError>;
    fn workflow_projection(&self, run_id: &str) -> Result<StructcopyWorkflow, StructcopyError>;
    fn path_roots(&self) -> StructcopyPathRoots;
    fn write_clipboard(&self, text: &str) -> Result<StructcopyTransport, String>;
}

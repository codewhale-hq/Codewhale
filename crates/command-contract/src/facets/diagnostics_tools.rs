//! Bounded, structured prepared-tool evidence for `/tools`.
//!
//! The host captures and classifies facts using the existing inspector.
//! These data-only projections retain its JSON schema, including explicit
//! unknown states. Text and JSON rendering remain portable and single-owned.

use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DebugBoundedString {
    pub value: String,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum DebugEvidence<T> {
    Known { value: T },
    Unknown { reason: String },
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DebugBoundedList {
    pub count: usize,
    pub rendered: Vec<DebugBoundedString>,
    pub omitted: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DebugCountOnly {
    pub count: usize,
    pub values: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DebugToolProvenance {
    Builtin,
    Plugin,
    Mcp,
    Synthetic,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DebugToolVisibility {
    Active,
    Deferred,
    InRequest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum DebugProviderAvailability {
    Unknown,
    Available { provider: String, model: String },
    Unavailable { reason: String },
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DebugToolProjection {
    pub ordinal: usize,
    pub name: DebugBoundedString,
    pub tool_type: DebugEvidence<DebugBoundedString>,
    pub description: DebugBoundedString,
    pub input_schema_json: DebugBoundedString,
    pub allowed_callers: DebugEvidence<DebugBoundedList>,
    pub defer_loading: DebugEvidence<bool>,
    pub input_examples: DebugEvidence<DebugCountOnly>,
    pub strict: DebugEvidence<bool>,
    pub cache_control_type: DebugEvidence<DebugBoundedString>,
    pub provenance: DebugEvidence<DebugToolProvenance>,
    pub mcp_server: DebugEvidence<DebugBoundedString>,
    pub capabilities: DebugEvidence<DebugBoundedList>,
    pub approval: DebugEvidence<DebugBoundedString>,
    pub model_visible: DebugEvidence<bool>,
    pub visibility: DebugToolVisibility,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DebugTurnOutcomeStatus {
    Completed,
    Interrupted,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DebugTurnStopReason {
    ProviderNoToolCall,
    ProviderToolCallMissing,
    StepBudgetExhausted,
    NoProgress,
    Interrupted,
    Failed,
}

/// Precise, terminal facts: every optional counter retains unknown vs zero.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DebugTurnStopDiagnostics {
    pub status: Option<DebugTurnOutcomeStatus>,
    pub reason: Option<DebugTurnStopReason>,
    pub effective_max_steps: Option<u32>,
    pub step_budget_source: String,
    pub model_step_index: u32,
    pub model_requests_started: u32,
    pub transport_retries: u32,
    pub transparent_stream_retries: u32,
    pub stream_resumes: u32,
    pub reasoning_only_reprompts: u32,
    pub empty_stop_retries: u32,
    pub final_report_requested: bool,
    pub permission_strategy_switches: u32,
    pub permission_denial_rounds_without_progress: u32,
    pub last_provider_finish_reason: Option<DebugBoundedString>,
    pub last_response_tool_calls: Option<usize>,
    pub last_response_tool_calls_suppressed: Option<usize>,
    pub last_reported_input_tokens: Option<u32>,
    pub route_context_window_tokens: Option<u64>,
    pub last_prepared_output_limit_tokens: Option<u32>,
    pub automatic_compaction_attempts: u32,
    pub emergency_compaction_attempts: u32,
}

/// One frozen prepared-request snapshot; no registry or client reference.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DebugToolSnapshot {
    pub schema_version: u32,
    pub capture_source: String,
    pub delivery_status: String,
    pub turn_id: DebugBoundedString,
    pub step: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminal: Option<DebugTurnStopDiagnostics>,
    pub tools_field_present: bool,
    pub tool_count: usize,
    pub rendered_tool_count: usize,
    pub omitted_tool_count: usize,
    pub payload_json_bytes: Option<usize>,
    pub payload_measurement_status: String,
    pub active_tool_catalog_sha256: Option<String>,
    pub unavailable_for_this_request: Vec<String>,
    pub provider: DebugProviderAvailability,
    pub registry_facts_present: bool,
    pub registry_tool_count: DebugEvidence<usize>,
    pub registry_only_tools: DebugEvidence<DebugBoundedList>,
    pub tools: Vec<DebugToolProjection>,
}

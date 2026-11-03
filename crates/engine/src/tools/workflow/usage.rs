use serde::{Deserialize, Serialize};

/// Per-worker usage telemetry carried on `task_completed` events (#2974).
///
/// Tokens come from the worker ledger (`AgentRunUsage`); `tool_calls` is the
/// worker's model/tool step count (`SubAgentResult::steps_taken`) and
/// `result_ref` points at the durable child artifact (transcript handle) so
/// consumers can fetch full output by reference instead of inline text.
/// Field names mirror `AgentRunUsage` so #4039 can render Tokens/Tools
/// columns without a remapping layer.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub(super) struct WorkflowTaskUsage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) output_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) total_tokens: Option<u64>,
    /// Priced USD subtotal carried from the worker's immutable route audits,
    /// in microdollars. Absence is unknown, never a zero-cost claim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) cost_microusd: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) tool_calls: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) result_ref: Option<String>,
    /// Provenance of the token counts. This producer currently emits only
    /// `provider_reported`; absent means unknown and must never render as zero
    /// (#4039).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) token_source: Option<WorkflowTokenSource>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(super) enum WorkflowTokenSource {
    ProviderReported,
}

/// Run-wide usage totals reconciled from per-task telemetry, carried on
/// `run_completed` events and the persisted run record (#2974).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub(super) struct WorkflowRunUsage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) output_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) total_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) cost_microusd: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) tool_calls: Option<u64>,
    /// Number of completed tasks that contributed telemetry.
    #[serde(default)]
    pub(super) tasks_reported: u64,
}

impl WorkflowRunUsage {
    pub(super) fn from_task(usage: &WorkflowTaskUsage) -> Self {
        Self {
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            total_tokens: usage.total_tokens,
            cost_microusd: usage.cost_microusd,
            tool_calls: usage.tool_calls.map(u64::from),
            tasks_reported: 1,
        }
    }

    pub(super) fn add_task(&mut self, usage: &WorkflowTaskUsage) {
        self.input_tokens = sum_optional_usage(self.input_tokens, usage.input_tokens);
        self.output_tokens = sum_optional_usage(self.output_tokens, usage.output_tokens);
        self.total_tokens = sum_optional_usage(self.total_tokens, usage.total_tokens);
        self.cost_microusd = sum_optional_usage(self.cost_microusd, usage.cost_microusd);
        self.tool_calls = sum_optional_usage(self.tool_calls, usage.tool_calls.map(u64::from));
        self.tasks_reported = self.tasks_reported.saturating_add(1);
    }
}

pub(super) fn sum_optional_usage(left: Option<u64>, right: Option<u64>) -> Option<u64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.saturating_add(right)),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    }
}

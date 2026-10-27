//! Value-only projection owned by the authoritative source module.
//! No command, App or rendering operation is reachable from these conversions.

use codewhale_command_contract::facets::*;

fn bounded(value: &crate::tool_inspection::BoundedString) -> DebugBoundedString {
    DebugBoundedString {
        value: value.value.clone(),
        truncated: value.truncated,
    }
}

fn evidence<T, U>(
    value: &crate::tool_inspection::Evidence<T>,
    convert: impl Fn(&T) -> U,
) -> DebugEvidence<U> {
    match value {
        crate::tool_inspection::Evidence::Known { value } => DebugEvidence::Known {
            value: convert(value),
        },
        crate::tool_inspection::Evidence::Unknown { reason } => DebugEvidence::Unknown {
            reason: reason.clone(),
        },
    }
}

fn bounded_list(value: &crate::tool_inspection::BoundedList) -> DebugBoundedList {
    DebugBoundedList {
        count: value.count,
        rendered: value.rendered.iter().map(bounded).collect(),
        omitted: value.omitted,
    }
}

pub(crate) fn tool_snapshot(
    value: &crate::tool_inspection::ToolInspectionSnapshot,
) -> DebugToolSnapshot {
    use crate::tool_inspection::{
        ProviderAvailability as P, ToolProvenance as R, ToolVisibility as V, TurnStopReason as S,
    };
    DebugToolSnapshot {
        schema_version: value.schema_version,
        capture_source: value.capture_source.to_string(),
        delivery_status: value.delivery_status.to_string(),
        turn_id: bounded(&value.turn_id),
        step: value.step,
        terminal: value
            .terminal
            .as_ref()
            .map(|terminal| DebugTurnStopDiagnostics {
                status: terminal.status.map(|status| match status {
                    crate::core::events::TurnOutcomeStatus::Completed => {
                        DebugTurnOutcomeStatus::Completed
                    }
                    crate::core::events::TurnOutcomeStatus::Interrupted => {
                        DebugTurnOutcomeStatus::Interrupted
                    }
                    crate::core::events::TurnOutcomeStatus::Failed => {
                        DebugTurnOutcomeStatus::Failed
                    }
                }),
                reason: terminal.reason.map(|reason| match reason {
                    S::ProviderNoToolCall => DebugTurnStopReason::ProviderNoToolCall,
                    S::ProviderToolCallMissing => DebugTurnStopReason::ProviderToolCallMissing,
                    S::StepBudgetExhausted => DebugTurnStopReason::StepBudgetExhausted,
                    S::NoProgress => DebugTurnStopReason::NoProgress,
                    S::Interrupted => DebugTurnStopReason::Interrupted,
                    S::Failed => DebugTurnStopReason::Failed,
                }),
                effective_max_steps: terminal.effective_max_steps,
                step_budget_source: terminal.step_budget_source.to_string(),
                model_step_index: terminal.model_step_index,
                model_requests_started: terminal.model_requests_started,
                transport_retries: terminal.transport_retries,
                transparent_stream_retries: terminal.transparent_stream_retries,
                stream_resumes: terminal.stream_resumes,
                reasoning_only_reprompts: terminal.reasoning_only_reprompts,
                empty_stop_retries: terminal.empty_stop_retries,
                final_report_requested: terminal.final_report_requested,
                permission_strategy_switches: terminal.permission_strategy_switches,
                permission_denial_rounds_without_progress: terminal
                    .permission_denial_rounds_without_progress,
                last_provider_finish_reason: terminal
                    .last_provider_finish_reason
                    .as_ref()
                    .map(bounded),
                last_response_tool_calls: terminal.last_response_tool_calls,
                last_response_tool_calls_suppressed: terminal.last_response_tool_calls_suppressed,
                last_reported_input_tokens: terminal.last_reported_input_tokens,
                route_context_window_tokens: terminal.route_context_window_tokens,
                last_prepared_output_limit_tokens: terminal.last_prepared_output_limit_tokens,
                automatic_compaction_attempts: terminal.automatic_compaction_attempts,
                emergency_compaction_attempts: terminal.emergency_compaction_attempts,
            }),
        tools_field_present: value.tools_field_present,
        tool_count: value.tool_count,
        rendered_tool_count: value.rendered_tool_count,
        omitted_tool_count: value.omitted_tool_count,
        payload_json_bytes: value.payload_json_bytes,
        payload_measurement_status: value.payload_measurement_status.clone(),
        active_tool_catalog_sha256: value.active_tool_catalog_sha256.clone(),
        unavailable_for_this_request: value
            .unavailable_for_this_request
            .iter()
            .map(|item| (*item).to_string())
            .collect(),
        provider: match &value.provider {
            P::Unknown => DebugProviderAvailability::Unknown,
            P::Available { provider, model } => DebugProviderAvailability::Available {
                provider: provider.clone(),
                model: model.clone(),
            },
            P::Unavailable { reason } => DebugProviderAvailability::Unavailable {
                reason: reason.clone(),
            },
        },
        registry_facts_present: value.registry_facts_present,
        registry_tool_count: evidence(&value.registry_tool_count, |count| *count),
        registry_only_tools: evidence(&value.registry_only_tools, bounded_list),
        tools: value
            .tools
            .iter()
            .map(|tool| DebugToolProjection {
                ordinal: tool.ordinal,
                name: bounded(&tool.name),
                tool_type: evidence(&tool.tool_type, bounded),
                description: bounded(&tool.description),
                input_schema_json: bounded(&tool.input_schema_json),
                allowed_callers: evidence(&tool.allowed_callers, bounded_list),
                defer_loading: evidence(&tool.defer_loading, |flag| *flag),
                input_examples: evidence(&tool.input_examples, |count| DebugCountOnly {
                    count: count.count,
                    values: count.values.to_string(),
                }),
                strict: evidence(&tool.strict, |flag| *flag),
                cache_control_type: evidence(&tool.cache_control_type, bounded),
                provenance: evidence(&tool.provenance, |origin| match origin {
                    R::Builtin => DebugToolProvenance::Builtin,
                    R::Plugin => DebugToolProvenance::Plugin,
                    R::Mcp => DebugToolProvenance::Mcp,
                    R::Synthetic => DebugToolProvenance::Synthetic,
                    R::Unknown => DebugToolProvenance::Unknown,
                }),
                mcp_server: evidence(&tool.mcp_server, bounded),
                capabilities: evidence(&tool.capabilities, bounded_list),
                approval: evidence(&tool.approval, bounded),
                model_visible: evidence(&tool.model_visible, |flag| *flag),
                visibility: match tool.visibility {
                    V::Active => DebugToolVisibility::Active,
                    V::Deferred => DebugToolVisibility::Deferred,
                    V::InRequest => DebugToolVisibility::InRequest,
                },
            })
            .collect(),
    }
}

//! Compile-enforced parity between the engine's internal `Op` / `Event` and
//! the protocol's `Op` / `EventMsg` (core/protocol extraction spec, Phase A1).
//!
//! Every function here is one exhaustive `match` with **no wildcard arm**.
//! Adding an engine `Event` or `Op` variant without a protocol twin fails to
//! compile right here — that is the `protocol_covers_engine_events` /
//! `protocol_covers_engine_ops` guard from spec §7. The `#[cfg(test)]`
//! block below only proves the projections agree with the protocol's own
//! wire-tag tables and that this file keeps its no-wildcard discipline.
//!
//! The reverse direction (protocol `Op` -> engine `Op`) is not total — engine
//! ops carry resolved routes, reply channels, and hook executors that the
//! host must supply — so it lands with the engine handle in Phase C/D, not
//! here.
//!
//! The foreground pet observer consumes the event projection, retaining only
//! lifecycle metadata. Other projections remain compile-time parity guards;
//! dead-code is allowed here rather than hiding those guards behind a test cfg
//! (which would let `cargo build` pass with an unmapped variant).
#![allow(dead_code)]

use std::collections::BTreeMap;

use codewhale_protocol::event_msg as wire;
use codewhale_protocol::ids::{SessionId, ThreadId};
use codewhale_protocol::op as wire_op;
use serde::Serialize;
use serde_json::Value;

use crate::agent_roster::{AgentRosterRow, RosterState};
use crate::compaction::CompactionConfig;
use crate::core::engine::preview::PreviewUnresolved;
use crate::core::events::{
    Event, RouteBillingEnvelope, ToolGate, ToolGateVerdict, TurnOutcomeStatus, TurnRoute,
};
use crate::core::ops::Op;
use crate::cost_status::RouteBillingMode;
use crate::mcp::{McpManagerSnapshot, McpServerCapabilityMetadata};
use crate::model_profile::SupportState;
use crate::route_billing::RouteProduct;
use crate::tools::spec::ToolError;
use crate::tools::subagent::AgentWorkerStatus;
use crate::tools::user_input::UserInputRequest;
use codewhale_config::AppMode;
use codewhale_config::ProviderKind;
use codewhale_execpolicy::ApprovalMode;
use codewhale_models::Usage;
use codewhale_protocol::ResponseChannel;

/// Routing ids the engine does not carry on each event; the emitter supplies
/// them once per session.
#[derive(Debug, Clone)]
pub struct ProtocolIds {
    pub thread_id: ThreadId,
    pub session_id: SessionId,
}

fn to_value<T: Serialize>(value: &T) -> Value {
    serde_json::to_value(value).unwrap_or(Value::Null)
}

fn count(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

/// Lossless mode label; round-trips through `AppMode::parse`.
#[must_use]
pub fn app_mode_str(mode: AppMode) -> &'static str {
    match mode {
        AppMode::Agent => "agent",
        AppMode::Plan => "plan",
        AppMode::Operate => "operate",
    }
}

#[must_use]
pub fn approval_mode_str(mode: ApprovalMode) -> &'static str {
    match mode {
        ApprovalMode::Auto => "auto",
        ApprovalMode::Bypass => "bypass",
        ApprovalMode::Suggest => "suggest",
        ApprovalMode::Never => "never",
    }
}

#[must_use]
pub fn worker_status_str(status: AgentWorkerStatus) -> &'static str {
    match status {
        AgentWorkerStatus::Queued => "queued",
        AgentWorkerStatus::Starting => "starting",
        AgentWorkerStatus::Running => "running",
        AgentWorkerStatus::WaitingForUser => "waiting_for_user",
        AgentWorkerStatus::ModelWait => "model_wait",
        AgentWorkerStatus::RunningTool => "running_tool",
        AgentWorkerStatus::Completed => "completed",
        AgentWorkerStatus::Failed => "failed",
        AgentWorkerStatus::Cancelled => "cancelled",
        AgentWorkerStatus::Interrupted => "interrupted",
    }
}

fn roster_state_str(state: RosterState) -> &'static str {
    match state {
        RosterState::Running => "running",
        RosterState::Waiting => "waiting",
        RosterState::Parked => "parked",
        RosterState::Done => "done",
        RosterState::Failed => "failed",
        RosterState::Cancelled => "cancelled",
    }
}

fn billing_mode_str(mode: RouteBillingMode) -> &'static str {
    match mode {
        RouteBillingMode::Metered => "metered",
        RouteBillingMode::Subscription => "subscription",
        RouteBillingMode::Local => "local",
        RouteBillingMode::Unknown => "unknown",
    }
}

fn support_state_str(state: SupportState) -> &'static str {
    match state {
        SupportState::Supported => "supported",
        SupportState::Unsupported => "unsupported",
        SupportState::Unknown => "unknown",
    }
}

fn capability_metadata_str(metadata: McpServerCapabilityMetadata) -> &'static str {
    match metadata {
        McpServerCapabilityMetadata::Advertised(_) => "advertised",
        McpServerCapabilityMetadata::LegacyFallback => "legacy_fallback",
        McpServerCapabilityMetadata::NotObserved => "not_observed",
    }
}

fn provider_str(provider: ProviderKind) -> String {
    provider.as_str().to_string()
}

fn usage_to_wire(usage: &Usage) -> wire::TokenUsage {
    wire::TokenUsage {
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        prompt_cache_hit_tokens: usage.prompt_cache_hit_tokens,
        prompt_cache_miss_tokens: usage.prompt_cache_miss_tokens,
        prompt_cache_write_tokens: usage.prompt_cache_write_tokens,
        reasoning_tokens: usage.reasoning_tokens,
        reasoning_replay_tokens: usage.reasoning_replay_tokens,
        code_execution_requests: usage
            .server_tool_use
            .as_ref()
            .and_then(|server| server.code_execution_requests),
        tool_search_requests: usage
            .server_tool_use
            .as_ref()
            .and_then(|server| server.tool_search_requests),
    }
}

fn route_product_to_wire(product: RouteProduct) -> wire::RouteProduct {
    match product {
        RouteProduct::Unproven => wire::RouteProduct::Unproven,
        RouteProduct::Subscription(label) => wire::RouteProduct::Subscription {
            label: label.to_string(),
        },
        RouteProduct::Metered => wire::RouteProduct::Metered,
    }
}

fn billing_to_wire(billing: &RouteBillingEnvelope) -> wire::RouteBillingEnvelope {
    wire::RouteBillingEnvelope {
        openrouter_vendor: billing
            .openrouter_vendor
            .as_deref()
            .map(codewhale_runtime::safe_label::sanitize_persisted_route_label),
        billing_surface: billing.billing_surface.clone(),
        endpoint_fingerprint: billing.endpoint_fingerprint.clone(),
        provider_live_pricing: billing
            .provider_live_pricing
            .as_ref()
            .map(to_value)
            .filter(|v| !v.is_null()),
        billing_mode: billing_mode_str(billing.billing_mode).to_string(),
        dispatched_at: billing.dispatched_at,
    }
}

fn route_to_wire(route: &TurnRoute) -> wire::TurnRoute {
    wire::TurnRoute {
        provider: provider_str(route.provider),
        provider_identity: route.provider_identity.clone(),
        model: route.model.clone(),
        auto_model: route.auto_model,
        receipt: route
            .receipt
            .as_ref()
            .map(|receipt| wire::TurnRouteReceipt {
                provider: provider_str(receipt.provider()),
                provider_identity: receipt.provider_identity().to_string(),
                wire_model: receipt.wire_model().to_string(),
                endpoint_identity: receipt.endpoint_identity().to_string(),
                credential_generation_present: !receipt.credential_generation().is_empty(),
            }),
        billing: route.billing.as_ref().map(billing_to_wire),
        base_url: route.base_url.clone(),
        billing_product: route_product_to_wire(route.billing_product),
    }
}

fn tool_error_to_wire(error: &ToolError) -> wire::ToolCallError {
    match error {
        ToolError::InvalidInput { message } => wire::ToolCallError::InvalidInput {
            message: message.clone(),
        },
        ToolError::MissingField { field } => wire::ToolCallError::MissingField {
            field: field.clone(),
        },
        ToolError::PathEscape { path } => wire::ToolCallError::PathEscape { path: path.clone() },
        ToolError::ExecutionFailed { message, .. } => wire::ToolCallError::ExecutionFailed {
            message: message.clone(),
        },
        ToolError::Timeout { seconds } => wire::ToolCallError::Timeout { seconds: *seconds },
        ToolError::Cancelled { message } => wire::ToolCallError::Cancelled {
            message: message.clone(),
        },
        ToolError::NotAvailable { message } => wire::ToolCallError::NotAvailable {
            message: message.clone(),
        },
        ToolError::PermissionDenied { message } => wire::ToolCallError::PermissionDenied {
            message: message.clone(),
        },
    }
}

fn gate_to_wire(gate: ToolGate) -> wire::ToolGate {
    match gate {
        ToolGate::AutoReviewDeterministic => wire::ToolGate::AutoReviewDeterministic,
        ToolGate::AutoReviewGuardian => wire::ToolGate::AutoReviewGuardian,
    }
}

fn verdict_to_wire(verdict: ToolGateVerdict) -> wire::ToolGateVerdict {
    match verdict {
        ToolGateVerdict::Allowed => wire::ToolGateVerdict::Allowed,
        ToolGateVerdict::Denied => wire::ToolGateVerdict::Denied,
        ToolGateVerdict::Unavailable => wire::ToolGateVerdict::Unavailable,
    }
}

fn outcome_status_to_wire(status: TurnOutcomeStatus) -> wire::TurnOutcomeStatus {
    match status {
        TurnOutcomeStatus::Completed => wire::TurnOutcomeStatus::Completed,
        TurnOutcomeStatus::Interrupted => wire::TurnOutcomeStatus::Interrupted,
        TurnOutcomeStatus::Failed => wire::TurnOutcomeStatus::Failed,
    }
}

fn mcp_snapshot_to_wire(snapshot: &McpManagerSnapshot) -> wire::McpManagerSnapshot {
    let item = |item: &crate::mcp::McpDiscoveredItem| wire::McpDiscoveredItem {
        name: item.name.clone(),
        model_name: item.model_name.clone(),
        description: item.description.clone(),
    };
    wire::McpManagerSnapshot {
        config_path: snapshot.config_path.clone(),
        config_exists: snapshot.config_exists,
        reload_required: snapshot.reload_required,
        servers: snapshot
            .servers
            .iter()
            .map(|server| wire::McpServerSnapshot {
                name: server.name.clone(),
                enabled: server.enabled,
                required: server.required,
                transport: server.transport.clone(),
                command_or_url: redacted_command_or_url(&server.command_or_url),
                connect_timeout: server.connect_timeout,
                execute_timeout: server.execute_timeout,
                read_timeout: server.read_timeout,
                connected: server.connected,
                error: server.error.clone(),
                capability_metadata: capability_metadata_str(server.capability_metadata)
                    .to_string(),
                tools: server.tools.iter().map(item).collect(),
                resources: server.resources.iter().map(item).collect(),
                prompts: server.prompts.iter().map(item).collect(),
            })
            .collect(),
    }
}

/// Sanitize an MCP server's configured target before it goes on the wire.
///
/// `command_or_url` is the raw configuration: either the configured URL, which
/// can carry userinfo (`https://user:token@host`) or query credentials, or a
/// stdio command line built as `command + " " + args.join(" ")`, whose args can
/// carry a token. That is acceptable in the local picker, where the only reader
/// is the person who configured it. This projection is not local -- it feeds
/// `EventMsg::McpSessionBoot`, which every SSE and stream-JSON consumer
/// receives and any frame-retaining log keeps.
///
/// URLs reuse the existing masking in `codewhale_secrets::sanitize::redact_url_for_display`. Stdio
/// keeps the program name and elides the arguments, because a secret there can
/// be positional and is not reliably recognizable by key.
fn redacted_command_or_url(raw: &str) -> String {
    // No `match` here on purpose: `projections_have_no_wildcard_arms` scans
    // this whole file for wildcard arms, and a `_ =>` would trip it even in a
    // helper that projects no engine variant.
    let trimmed = raw.trim();
    // A URL never contains whitespace; an argv line does. `://` alone is not
    // enough to route here: `redact_url_for_display` returns its input
    // verbatim when `Url::parse` fails, so a stdio command that merely
    // mentions a URL — `docker run -e TOKEN=… img --url https://…` — would
    // reach the wire with its positional secret intact.
    if !trimmed.is_empty() && !trimmed.contains(char::is_whitespace) && trimmed.contains("://") {
        return codewhale_secrets::sanitize::redact_url_for_display(trimmed);
    }
    if let Some((program, rest)) = trimmed.split_once(char::is_whitespace)
        && !rest.trim().is_empty()
    {
        return format!("{program} …");
    }
    trimmed.to_string()
}

fn roster_row_to_wire(row: &AgentRosterRow) -> wire::AgentRosterRow {
    wire::AgentRosterRow {
        worker_id: row.worker_id.clone(),
        display_name: row.display_name.clone(),
        model: row.model.clone(),
        state: roster_state_str(row.state).to_string(),
        status: worker_status_str(row.status).to_string(),
        activity: row.activity.clone(),
        millis: row.millis,
        input_tokens: row.input_tokens,
        output_tokens: row.output_tokens,
        cost_microusd: row.cost_microusd,
        steps_taken: row.steps_taken,
        parent_run_id: row.parent_run_id.clone(),
        run_id: row.run_id.clone(),
    }
}

fn user_input_to_wire(request: &UserInputRequest) -> wire::UserInputRequest {
    wire::UserInputRequest {
        require_typed_answers: request.require_typed_answers,
        questions: request
            .questions
            .iter()
            .map(|question| codewhale_protocol::UserInputQuestionEvent {
                header: question.header.clone(),
                id: question.id.clone(),
                question: question.question.clone(),
                detail: question.detail.clone(),
                intent: question.intent.as_ref().map(|intent| match intent {
                    crate::tools::user_input::UserInputIntent::PlanReview { approve, call_id } => {
                        codewhale_protocol::UserInputIntentEvent::PlanReview {
                            approve: approve.clone(),
                            call_id: call_id.clone(),
                        }
                    }
                }),
                options: question
                    .options
                    .iter()
                    .map(|option| codewhale_protocol::UserInputOptionEvent {
                        label: option.label.clone(),
                        description: option.description.clone(),
                    })
                    .collect(),
                allow_free_text: question.allow_free_text,
                multi_select: question.multi_select,
            })
            .collect(),
    }
}

fn compaction_to_wire(config: &CompactionConfig) -> wire_op::CompactionPolicy {
    wire_op::CompactionPolicy {
        enabled: config.enabled,
        token_threshold: count(config.token_threshold),
        model: config.model.clone(),
        image_input: support_state_str(config.image_input).to_string(),
        effective_context_window: config.effective_context_window,
        cache_summary: config.cache_summary,
        focus: config.focus.clone(),
        runtime_cost_owner: config.runtime_cost_owner.clone(),
        workspace: config.workspace.clone(),
    }
}

/// Project the engine's per-turn authority onto the wire `TurnSpec`. Host-only
/// fields (`initial_routed_usage`, `hook_executor`) and resolved routes are
/// stripped; only their non-secret receipts cross.
fn turn_spec_to_wire(spec: &crate::core::ops::TurnSpec) -> wire_op::TurnSpec {
    // `spec.submission_id` is deliberately not projected: the token is
    // host-process-local by design and the wire op has no correlation twin,
    // so wire submitters observe `TurnStarted.submission_id` always absent
    // and cannot correlate submissions on that channel.
    wire_op::TurnSpec {
        profile_constitution: spec
            .profile_constitution
            .as_ref()
            .map(|snapshot| serde_json::json!(snapshot)),
        max_output_tokens: spec.max_output_tokens,
        content: spec.content.clone(),
        images: spec.images.clone(),
        mode: app_mode_str(spec.mode).to_string(),
        model: Some(spec.route.model.clone()),
        model_provider: Some(spec.route.identity.key.to_string()),
        allowed_tools: spec.allowed_tools.clone(),
        dynamic_tools: spec.dynamic_tools.clone(),
        provenance: spec.provenance.as_str().to_string(),
        compaction: Some(Box::new(compaction_to_wire(&spec.compaction))),
        goal_objective: spec.goal_objective.clone(),
        goal_token_budget: spec.goal_token_budget,
        goal_status: spec.goal_status.as_str().to_string(),
        reasoning_effort: spec.reasoning_effort.clone(),
        reasoning_effort_auto: spec.reasoning_effort_auto,
        auto_model: spec.auto_model,
        allow_shell: spec.allow_shell,
        trust_mode: spec.trust_mode,
        auto_approve: spec.auto_approve,
        approval_mode: approval_mode_str(spec.approval_mode).to_string(),
        translation_enabled: spec.translation_enabled,
        verbosity: spec.verbosity.clone(),
    }
}

fn preview_unresolved_str(unresolved: &PreviewUnresolved) -> String {
    match unresolved {
        PreviewUnresolved::AutoRouteNeedsPrompt => "auto_route_needs_prompt".to_string(),
        PreviewUnresolved::AutoRouteClassificationNotExecuted => {
            "auto_route_classification_not_executed".to_string()
        }
        PreviewUnresolved::NoPrompt => "no_prompt".to_string(),
        PreviewUnresolved::PlanFailed(error) => format!("plan_failed: {error}"),
        PreviewUnresolved::MessageSubmitHooksConfigured => {
            "message_submit_hooks_configured".to_string()
        }
        PreviewUnresolved::PromptResolutionFailed(error) => {
            format!("prompt_resolution_failed: {error}")
        }
    }
}

/// Project one engine event onto the protocol. Exhaustive: a new engine
/// variant without a protocol twin does not compile.
#[must_use]
pub fn event_to_protocol(event: &Event, ids: &ProtocolIds) -> wire::EventMsg {
    let thread_id = ids.thread_id.clone();
    let session_id = ids.session_id.clone();
    match event {
        Event::ToolProjectionWarning {
            provider,
            omitted_tool_names,
            omitted_tool_count,
        } => wire::EventMsg::ToolProjectionWarning {
            thread_id,
            session_id,
            provider: provider.clone(),
            omitted_tool_names: omitted_tool_names.clone(),
            omitted_tool_count: count(*omitted_tool_count),
        },
        Event::RequestImageWindow {
            provider,
            model,
            total,
            elided,
            cap,
            cap_source,
            message,
        } => wire::EventMsg::RequestImageWindow {
            thread_id,
            session_id,
            provider: provider.clone(),
            model: model.clone(),
            total: count(*total),
            elided: count(*elided),
            cap: count(*cap),
            cap_source: cap_source.clone(),
            message: message.clone(),
        },
        Event::SnapshotsDisabled { workspace, reason } => wire::EventMsg::SnapshotsDisabled {
            thread_id,
            session_id,
            workspace: workspace.clone(),
            reason: reason.clone(),
        },
        Event::MessageStarted { index } => wire::EventMsg::MessageStarted {
            thread_id,
            session_id,
            index: count(*index),
        },
        Event::MessageDelta { index, content } => wire::EventMsg::ResponseDelta {
            thread_id,
            session_id,
            index: count(*index),
            delta: content.clone(),
            channel: ResponseChannel::Text,
        },
        Event::MessageComplete { index } => wire::EventMsg::MessageComplete {
            thread_id,
            session_id,
            index: count(*index),
        },
        Event::ThinkingStarted { index } => wire::EventMsg::ThinkingStarted {
            thread_id,
            session_id,
            index: count(*index),
        },
        Event::ThinkingDelta { index, content } => wire::EventMsg::ResponseDelta {
            thread_id,
            session_id,
            index: count(*index),
            delta: content.clone(),
            channel: ResponseChannel::Reasoning,
        },
        Event::ThinkingComplete { index } => wire::EventMsg::ThinkingComplete {
            thread_id,
            session_id,
            index: count(*index),
        },
        Event::ToolExecutionStarted { id } => wire::EventMsg::ToolExecutionStarted {
            thread_id,
            session_id,
            tool_call_id: id.clone(),
        },
        Event::ToolResultContent { id, blocks } => wire::EventMsg::ToolResultContent {
            thread_id,
            session_id,
            tool_call_id: id.clone(),
            blocks: to_value(blocks),
        },
        Event::ToolCallStarted {
            id, name, input, ..
        } => wire::EventMsg::ToolCallStarted {
            thread_id,
            session_id,
            tool_call_id: id.clone(),
            tool_name: name.clone(),
            input: input.clone(),
        },
        Event::ToolCallHeartbeat => wire::EventMsg::ToolCallHeartbeat {
            thread_id,
            session_id,
        },
        Event::ToolCallComplete {
            id, name, result, ..
        } => wire::EventMsg::ToolCallComplete {
            thread_id,
            session_id,
            tool_call_id: id.clone(),
            tool_name: name.clone(),
            result: match result {
                Ok(result) => wire::ToolCallOutcome::Ok {
                    content: result.content.clone(),
                    success: result.success,
                    metadata: result.metadata.clone(),
                },
                Err(error) => wire::ToolCallOutcome::Err {
                    error: tool_error_to_wire(error),
                },
            },
        },
        Event::OperationActivityStarted {
            span_id,
            activity_kind,
            action_id,
        } => wire::EventMsg::OperationActivityStarted {
            thread_id,
            session_id,
            span_id: span_id.clone(),
            activity_kind: *activity_kind,
            action_id: action_id.clone(),
        },
        Event::OperationActivityCompleted {
            span_id,
            activity_kind,
            action_id,
            outcome,
        } => wire::EventMsg::OperationActivityCompleted {
            thread_id,
            session_id,
            span_id: span_id.clone(),
            activity_kind: *activity_kind,
            action_id: action_id.clone(),
            outcome: *outcome,
        },
        Event::TurnStarted {
            turn_id,
            created_at,
            route,
            submission_id,
        } => wire::EventMsg::TurnStarted {
            thread_id,
            session_id,
            turn_id: turn_id.clone(),
            created_at: *created_at,
            route: route.as_ref().map(route_to_wire),
            submission_id: submission_id.clone(),
        },
        Event::ToolRequestSnapshot { snapshot } => wire::EventMsg::ToolRequestSnapshot {
            thread_id,
            session_id,
            snapshot: to_value(snapshot),
        },
        Event::WorkspaceSnapshotTaken { snapshot } => wire::EventMsg::WorkspaceSnapshotTaken {
            thread_id,
            session_id,
            snapshot: to_value(snapshot),
        },
        Event::RouteDispatched { turn_id, route } => wire::EventMsg::RouteDispatched {
            thread_id,
            session_id,
            turn_id: turn_id.clone(),
            route: route_to_wire(route),
        },
        Event::TurnComplete {
            turn_id,
            submission_id,
            compaction_id,
            usage,
            parent_route_usage,
            routed_usage_dropped_records,
            status,
            error,
            tool_catalog,
            base_url,
        } => wire::EventMsg::TurnComplete {
            thread_id,
            session_id,
            turn_id: turn_id.clone(),
            submission_id: submission_id.clone(),
            compaction_id: compaction_id.clone(),
            status: outcome_status_to_wire(*status),
            error: error.clone(),
            usage: usage_to_wire(usage),
            parent_route_usage: Some(usage_to_wire(parent_route_usage)),
            routed_usage_dropped_records: *routed_usage_dropped_records,
            tool_catalog: tool_catalog
                .as_ref()
                .map(|tools| tools.iter().map(to_value).collect()),
            base_url: base_url.clone(),
        },
        Event::ProviderRequestReceipt { receipt } => wire::EventMsg::ProviderRequestReceipt {
            thread_id,
            session_id,
            receipt: receipt.clone(),
        },
        Event::TurnUsage {
            max_output_tokens,
            usage,
            duration_ms,
            first_token_ms,
            request_ms,
            finish_reason: _,
        } => wire::EventMsg::TurnUsage {
            max_output_tokens: *max_output_tokens,
            thread_id,
            session_id,
            usage: usage_to_wire(usage),
            duration_ms: *duration_ms,
            first_token_ms: *first_token_ms,
            request_ms: *request_ms,
        },
        Event::RoutedTurnUsage {
            usage,
            duration_ms,
            first_token_ms,
            request_ms,
        } => wire::EventMsg::RoutedTurnUsage {
            thread_id,
            session_id,
            usage: usage_to_wire(usage),
            duration_ms: *duration_ms,
            first_token_ms: *first_token_ms,
            request_ms: *request_ms,
        },
        Event::GoalUpdated { snapshot } => wire::EventMsg::GoalUpdated {
            thread_id,
            session_id,
            snapshot: to_value(snapshot),
        },
        Event::GoalContinuationWaiting { delay_seconds } => {
            wire::EventMsg::GoalContinuationWaiting {
                thread_id,
                session_id,
                delay_seconds: *delay_seconds,
            }
        }
        Event::GoalContinuationWaitEnded { interrupted } => {
            wire::EventMsg::GoalContinuationWaitEnded {
                thread_id,
                session_id,
                interrupted: *interrupted,
            }
        }
        Event::CompactionStarted { id, auto, message } => wire::EventMsg::CompactionStarted {
            thread_id,
            session_id,
            id: id.clone(),
            auto: *auto,
            message: message.clone(),
        },
        Event::CompactionCompleted {
            id,
            auto,
            message,
            messages_before,
            messages_after,
            summary_prompt,
            post_input_tokens,
        } => wire::EventMsg::CompactionCompleted {
            thread_id,
            session_id,
            id: id.clone(),
            auto: *auto,
            message: message.clone(),
            messages_before: messages_before.map(count),
            messages_after: messages_after.map(count),
            summary_prompt: summary_prompt.clone(),
            post_input_tokens: *post_input_tokens,
        },
        Event::CompactionCancelled { id, auto, message } => wire::EventMsg::CompactionCancelled {
            thread_id,
            session_id,
            id: id.clone(),
            auto: *auto,
            message: message.clone(),
        },
        Event::PurgeStarted { message } => wire::EventMsg::PurgeStarted {
            thread_id,
            session_id,
            message: message.clone(),
        },
        Event::PurgeCompleted {
            messages_before,
            messages_after,
            removed_count,
            replaced_count,
            message,
        } => wire::EventMsg::PurgeCompleted {
            thread_id,
            session_id,
            messages_before: count(*messages_before),
            messages_after: count(*messages_after),
            removed_count: count(*removed_count),
            replaced_count: count(*replaced_count),
            message: message.clone(),
        },
        Event::PurgeFailed { message } => wire::EventMsg::PurgeFailed {
            thread_id,
            session_id,
            message: message.clone(),
        },
        Event::CompactionFailed { id, auto, message } => wire::EventMsg::CompactionFailed {
            thread_id,
            session_id,
            id: id.clone(),
            auto: *auto,
            message: message.clone(),
        },
        Event::AgentSpawned {
            owner_session_id,
            id,
            prompt,
            worker_status,
            parent_run_id,
            spawn_depth,
            model,
            route_source,
            display_name,
        } => wire::EventMsg::AgentSpawned {
            thread_id,
            session_id,
            owner_session_id: owner_session_id.clone(),
            id: id.clone(),
            prompt: prompt.clone(),
            worker_status: worker_status.map(|status| worker_status_str(status).to_string()),
            parent_run_id: parent_run_id.clone(),
            spawn_depth: *spawn_depth,
            model: model.clone(),
            route_source: route_source.clone(),
            display_name: display_name.clone(),
        },
        Event::AgentProgress {
            owner_session_id,
            id,
            status,
            activity,
            parent_run_id,
            spawn_depth,
        } => wire::EventMsg::AgentProgress {
            thread_id,
            session_id,
            owner_session_id: owner_session_id.clone(),
            id: id.clone(),
            status: status.clone(),
            activity: wire::AgentProgressActivity {
                worker_status: worker_status_str(activity.worker_status).to_string(),
                step: activity.step,
                tool_name: activity.tool_name.clone(),
            },
            parent_run_id: parent_run_id.clone(),
            spawn_depth: *spawn_depth,
        },
        Event::AgentComplete {
            owner_session_id,
            id,
            result,
            outcome,
            parent_run_id,
            spawn_depth,
            continuable,
            // Child usage stays off the wire: no protocol client consumes
            // it, and metrics reads the persisted runtime payload (#6315).
            usage: _,
            display_name,
        } => wire::EventMsg::AgentComplete {
            thread_id,
            session_id,
            owner_session_id: owner_session_id.clone(),
            id: id.clone(),
            result: result.clone(),
            worker_status: outcome
                .as_ref()
                .map(|status| crate::tools::subagent::subagent_status_name(status).to_string()),
            parent_run_id: parent_run_id.clone(),
            spawn_depth: *spawn_depth,
            continuable: *continuable,
            display_name: display_name.clone(),
        },
        Event::SubAgentFollowUp {
            owner_session_id,
            agent_id,
            outcome,
        } => wire::EventMsg::SubAgentFollowUp {
            thread_id,
            session_id,
            owner_session_id: owner_session_id.clone(),
            agent_id: agent_id.clone(),
            outcome: match outcome {
                Ok(outcome) => wire::SubAgentFollowUpOutcome::Ok {
                    agent_id: outcome.agent_id.clone(),
                    target_agent_id: outcome.target_agent_id.clone(),
                    delivered: outcome.delivered,
                    resumed: outcome.resumed,
                    note: outcome.note.clone(),
                },
                Err(reason) => wire::SubAgentFollowUpOutcome::Err {
                    reason: reason.clone(),
                },
            },
        },
        Event::AgentList {
            owner_session_id,
            agents,
            coordination,
            queued_follow_ups,
            roster,
        } => wire::EventMsg::AgentList {
            thread_id,
            session_id,
            owner_session_id: owner_session_id.clone(),
            agents: agents.iter().map(to_value).collect(),
            coordination: to_value(coordination),
            queued_follow_ups: queued_follow_ups
                .iter()
                .map(|(agent_id, queued)| (agent_id.clone(), count(*queued)))
                .collect::<BTreeMap<_, _>>(),
            roster: roster.iter().map(roster_row_to_wire).collect(),
        },
        Event::SubAgentMailbox {
            owner_session_id,
            turn_id,
            seq,
            message,
        } => wire::EventMsg::SubAgentMailbox {
            thread_id,
            session_id,
            owner_session_id: owner_session_id.clone(),
            turn_id: turn_id.clone(),
            seq: *seq,
            message: to_value(message),
        },
        Event::WorkflowUi {
            owner_session_id,
            run_id,
            event,
        } => wire::EventMsg::WorkflowUi {
            thread_id,
            session_id,
            owner_session_id: owner_session_id.clone(),
            run_id: run_id.clone(),
            ui_event: event.clone(),
        },
        Event::Error {
            envelope,
            recoverable,
        } => wire::EventMsg::Error {
            thread_id,
            session_id,
            category: envelope.category.to_string(),
            severity: envelope.severity.to_string(),
            recoverable: *recoverable,
            code: envelope.code.clone(),
            message: envelope.message.clone(),
        },
        Event::Status { message } => wire::EventMsg::Status {
            thread_id,
            session_id,
            message: message.clone(),
        },
        Event::McpSessionBoot {
            generation,
            snapshot,
            connecting,
            finished,
        } => wire::EventMsg::McpSessionBoot {
            thread_id,
            session_id,
            generation: *generation,
            snapshot: mcp_snapshot_to_wire(snapshot),
            connecting: connecting.clone(),
            finished: *finished,
        },
        Event::RequestManifestReady { rendered } => wire::EventMsg::RequestManifestReady {
            thread_id,
            session_id,
            rendered: rendered.clone(),
        },
        // The in-process `ack` notifier is an engine handle, not wire data.
        Event::PauseEvents { ack: _ } => wire::EventMsg::PauseEvents {
            thread_id,
            session_id,
        },
        Event::ResumeEvents => wire::EventMsg::ResumeEvents {
            thread_id,
            session_id,
        },
        Event::ApprovalRequired {
            id,
            tool_name,
            description,
            input,
            approval_key,
            approval_grouping_key,
            intent_summary,
            approval_force_prompt,
        } => wire::EventMsg::ApprovalRequired {
            thread_id,
            session_id,
            id: id.clone(),
            tool_name: tool_name.clone(),
            description: description.clone(),
            input: input.clone(),
            approval_key: approval_key.clone(),
            approval_grouping_key: approval_grouping_key.clone(),
            intent_summary: intent_summary.clone(),
            approval_force_prompt: *approval_force_prompt,
        },
        Event::ApprovalWithdrawn { id } => wire::EventMsg::ApprovalWithdrawn {
            thread_id,
            session_id,
            id: id.clone(),
        },
        Event::UserInputRequired { id, request } => wire::EventMsg::UserInputRequired {
            thread_id,
            session_id,
            id: id.clone(),
            request: user_input_to_wire(request),
        },
        Event::UserInputClosed { id, outcome } => wire::EventMsg::UserInputClosed {
            thread_id,
            session_id,
            id: id.clone(),
            outcome: match outcome {
                crate::core::events::UserInputClosedOutcome::Answered => {
                    wire::UserInputClosedOutcome::Answered
                }
                crate::core::events::UserInputClosedOutcome::Cancelled => {
                    wire::UserInputClosedOutcome::Cancelled
                }
                crate::core::events::UserInputClosedOutcome::TimedOut => {
                    wire::UserInputClosedOutcome::TimedOut
                }
                crate::core::events::UserInputClosedOutcome::Unavailable => {
                    wire::UserInputClosedOutcome::Unavailable
                }
            },
        },
        Event::SessionUpdated {
            checkpoint: _,
            session_id: engine_session_id,
            messages,
            system_prompt,
            instruction_assembly,
            model,
            workspace,
        } => wire::EventMsg::SessionUpdated {
            thread_id,
            session_id,
            engine_session_id: engine_session_id.clone(),
            messages: messages.iter().map(to_value).collect(),
            system_prompt: system_prompt.as_ref().map(to_value),
            instruction_assembly: instruction_assembly
                .as_deref()
                .map(crate::project_context::InstructionAssembly::receipt_json),
            model: model.clone(),
            workspace: workspace.clone(),
        },
        Event::ElevationRequired {
            tool_id,
            tool_name,
            command,
            denial_reason,
            blocked_network,
            blocked_write,
        } => wire::EventMsg::ElevationRequired {
            thread_id,
            session_id,
            tool_id: tool_id.clone(),
            tool_name: tool_name.clone(),
            command: command.clone(),
            denial_reason: denial_reason.clone(),
            blocked_network: *blocked_network,
            blocked_write: *blocked_write,
        },
        Event::LspRepairUpdate {
            diagnostics_found,
            files,
            injected,
        } => wire::EventMsg::LspRepairUpdate {
            thread_id,
            session_id,
            diagnostics_found: count(*diagnostics_found),
            files: count(*files),
            injected: *injected,
        },
        Event::ToolGateDecision {
            agent_id,
            tool_id,
            tool_name,
            gate,
            decision,
            risk,
            reason,
        } => wire::EventMsg::ToolGateDecision {
            thread_id,
            session_id,
            agent_id: agent_id.clone(),
            tool_id: tool_id.clone(),
            tool_name: tool_name.clone(),
            gate: gate_to_wire(*gate),
            decision: verdict_to_wire(*decision),
            risk: risk.clone(),
            reason: reason.clone(),
        },
        Event::AdvisoryNote {
            turn_id,
            note,
            tool_call_count,
        } => wire::EventMsg::AdvisoryNote {
            thread_id,
            session_id,
            turn_id: turn_id.clone(),
            note: note.clone(),
            tool_call_count: *tool_call_count,
        },
        Event::PrefixCacheChange {
            description,
            system_prompt_changed,
            tools_changed,
            stability_pct,
            changed,
            pinned_combined_hash,
            pin_reason,
            last_miss_reason,
            context_updates,
        } => wire::EventMsg::PrefixCacheChange {
            thread_id,
            session_id,
            description: description.clone(),
            system_prompt_changed: *system_prompt_changed,
            tools_changed: *tools_changed,
            stability_pct: *stability_pct,
            changed: *changed,
            pinned_combined_hash: pinned_combined_hash.clone(),
            pin_reason: pin_reason.clone(),
            last_miss_reason: last_miss_reason.clone(),
            context_updates: *context_updates,
        },
    }
}

/// Project one engine op onto the protocol. Exhaustive: a new engine variant
/// without a protocol twin does not compile. Reply channels, hook executors,
/// and resolved clients are stripped; only their non-secret receipts cross.
#[must_use]
pub fn op_to_protocol(op: &Op) -> wire_op::Op {
    match op {
        Op::Review { turn, request } => wire_op::Op::Review {
            turn: turn_spec_to_wire(turn),
            request: request.clone(),
        },
        Op::SendMessage(spec) => wire_op::Op::SendMessage(turn_spec_to_wire(spec)),
        Op::ContinueGoal {
            dynamic_tools,
            engine_schedule_id,
        } => wire_op::Op::ContinueGoal {
            dynamic_tools: dynamic_tools.clone(),
            engine_schedule_id: *engine_schedule_id,
        },
        Op::RunShellCommand {
            command,
            mode,
            allow_shell,
            trust_mode,
            auto_approve,
            approval_mode,
        } => wire_op::Op::RunShellCommand {
            command: command.clone(),
            mode: app_mode_str(*mode).to_string(),
            allow_shell: *allow_shell,
            trust_mode: *trust_mode,
            auto_approve: *auto_approve,
            approval_mode: approval_mode_str(*approval_mode).to_string(),
        },
        Op::SetGoalStatus {
            status,
            clear,
            goal_id,
        } => wire_op::Op::SetGoalStatus {
            goal_id: goal_id.clone(),
            status: status.as_str().to_string(),
            clear: *clear,
        },
        Op::SetGoalObjective {
            objective,
            token_budget,
            goal_id,
        } => wire_op::Op::SetGoalObjective {
            goal_id: goal_id.clone(),
            objective: objective.clone(),
            token_budget: *token_budget,
        },
        Op::PreviewOutboundRequest {
            inputs,
            json,
            base_prompt_only,
        } => wire_op::Op::PreviewOutboundRequest {
            json: *json,
            base_prompt_only: *base_prompt_only,
            mode: app_mode_str(inputs.mode).to_string(),
            allow_shell: inputs.allow_shell,
            trust_mode: inputs.trust_mode,
            auto_approve: inputs.auto_approve,
            approval_mode: approval_mode_str(inputs.approval_mode).to_string(),
            allowed_tools: inputs.allowed_tools.clone(),
            dynamic_tools: inputs.dynamic_tools.clone(),
            provenance: inputs.provenance.as_str().to_string(),
            requested_model: inputs.requested_model.clone(),
            requested_reasoning: inputs.requested_reasoning.clone(),
            auto_model: inputs.auto_model,
            hypothetical_prompt_supplied: inputs.hypothetical_prompt_supplied,
            hypothetical_prompt: inputs.next_turn.as_ref().map(|turn| turn.content.clone()),
            unresolved: if inputs.next_turn.is_some() {
                None
            } else {
                Some(preview_unresolved_str(&inputs.unresolved))
            },
        },
        Op::ListSubAgents => wire_op::Op::ListSubAgents,
        Op::GetSubAgentSettlement { tx: _ } => wire_op::Op::GetSubAgentSettlement,
        Op::CancelSubAgent { agent_id } => wire_op::Op::CancelSubAgent {
            agent_id: agent_id.clone(),
        },
        Op::FollowUpSubAgent { agent_id, text } => wire_op::Op::FollowUpSubAgent {
            agent_id: agent_id.clone(),
            text: text.clone(),
        },
        Op::StartFleetMember { start, tx: _ } => wire_op::Op::StartFleetMember {
            member_id: start.member_id.clone(),
        },
        Op::ChangeMode {
            mode,
            allow_shell,
            trust_mode,
            auto_approve,
            approval_mode,
            configured_sandbox_mode,
        } => wire_op::Op::ChangeMode {
            mode: app_mode_str(*mode).to_string(),
            allow_shell: *allow_shell,
            trust_mode: *trust_mode,
            auto_approve: *auto_approve,
            approval_mode: approval_mode_str(*approval_mode).to_string(),
            configured_sandbox_mode: configured_sandbox_mode.clone(),
        },
        Op::SetModel {
            model,
            mode,
            route_limits,
        } => wire_op::Op::SetModel {
            model: model.clone(),
            mode: app_mode_str(*mode).to_string(),
            route_limits: route_limits.map(|limits| wire_op::RouteLimits {
                context_tokens: limits.context_tokens,
                input_tokens: limits.input_tokens,
                output_tokens: limits.output_tokens,
            }),
        },
        Op::SetCompaction { config } => wire_op::Op::SetCompaction {
            config: compaction_to_wire(config),
        },
        Op::SetStreamChunkTimeout { timeout_secs } => wire_op::Op::SetStreamChunkTimeout {
            timeout_secs: *timeout_secs,
        },
        Op::SetSubagentRuntimeConfig {
            enabled,
            max_subagents,
            launch_concurrency,
            max_spawn_depth,
            api_timeout_secs,
            heartbeat_timeout_secs,
        } => wire_op::Op::SetSubagentRuntimeConfig {
            enabled: *enabled,
            max_subagents: count(*max_subagents),
            launch_concurrency: count(*launch_concurrency),
            max_spawn_depth: *max_spawn_depth,
            api_timeout_secs: *api_timeout_secs,
            heartbeat_timeout_secs: *heartbeat_timeout_secs,
        },
        Op::SetSearchProvider { provider } => wire_op::Op::SetSearchProvider {
            provider: provider.as_str().to_string(),
        },
        Op::SetFleetRoster { roster } => wire_op::Op::SetFleetRoster {
            member_ids: roster
                .members()
                .iter()
                .map(|member| member.id.clone())
                .collect(),
            exact_selection: roster.is_exact_selection(),
            load_error: roster.load_error().map(str::to_string),
        },
        Op::SyncSession {
            session_id,
            messages,
            system_prompt,
            system_prompt_override,
            model,
            workspace,
            mode,
        } => wire_op::Op::SyncSession {
            engine_session_id: session_id.clone(),
            messages: messages.iter().map(to_value).collect(),
            system_prompt: system_prompt.as_ref().map(to_value),
            system_prompt_override: *system_prompt_override,
            model: model.clone(),
            workspace: workspace.clone(),
            mode: app_mode_str(*mode).to_string(),
        },
        Op::RewindConversation {
            expected,
            messages,
            tx: _,
        } => wire_op::Op::RewindConversation {
            expected: to_value(expected),
            messages: messages.iter().map(to_value).collect(),
        },
        Op::CompactContext {
            id,
            route,
            compaction,
        } => wire_op::Op::CompactContext {
            id: id.clone(),
            model: route.model.clone(),
            model_provider: route.identity.key.to_string(),
            compaction: compaction_to_wire(compaction),
        },
        Op::CancelCompaction { id } => wire_op::Op::CancelCompaction { id: id.clone() },
        // Reply channels never cross the wire: the answer is a frame.
        Op::GetSessionSnapshot { tx: _ } => wire_op::Op::GetSessionSnapshot,
        Op::GetContextBudget { tx: _ } => wire_op::Op::GetContextBudget,
        Op::GetProviderRuntimeStatus { tx: _ } => wire_op::Op::GetProviderRuntimeStatus,
        Op::BootstrapMcp { tx: _ } => wire_op::Op::BootstrapMcp,
        Op::RetryMcpServer { name, tx: _ } => wire_op::Op::RetryMcpServer { name: name.clone() },
        Op::ReloadMcp { config_path, tx: _ } => wire_op::Op::ReloadMcp {
            config_path: config_path.clone(),
        },
        Op::PurgeContext => wire_op::Op::PurgeContext,
        Op::EditLastTurn {
            new_message,
            submission_id: _,
        } => wire_op::Op::EditLastTurn {
            new_message: new_message.clone(),
        },
        Op::SetAdvisorEnabled { enabled } => wire_op::Op::SetAdvisorEnabled { enabled: *enabled },
        Op::Shutdown => wire_op::Op::Shutdown,
    }
}

impl Event {
    /// Protocol projection of this event. See [`event_to_protocol`].
    #[must_use]
    pub fn to_protocol(&self, ids: &ProtocolIds) -> wire::EventMsg {
        event_to_protocol(self, ids)
    }
}

impl Op {
    /// Protocol projection of this op. See [`op_to_protocol`].
    #[must_use]
    pub fn to_protocol(&self) -> wire_op::Op {
        op_to_protocol(self)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn mcp_command_or_url_is_redacted_before_it_reaches_the_wire() {
        // Local display may show the configured value; this projection feeds
        // EventMsg::McpSessionBoot, which every SSE/stream-JSON consumer sees.
        assert_eq!(
            redacted_command_or_url("https://user:tok@mcp.example.com/sse"),
            "https://***:***@mcp.example.com/sse"
        );
        let masked = redacted_command_or_url("https://mcp.example.com/sse?api_key=SECRET");
        assert!(!masked.contains("SECRET"), "{masked}");
        // stdio: keep the program, drop the args -- a token there can be
        // positional, so key-based masking is not enough.
        assert_eq!(
            redacted_command_or_url("npx -y server --token SECRET"),
            "npx …"
        );
        // Nothing to hide, nothing changed.
        assert_eq!(redacted_command_or_url("npx"), "npx");
        // A stdio command that merely mentions a URL must still collapse to
        // the program name. Routing it to the URL redactor returns it verbatim
        // (`Url::parse` rejects the spaces), leaking the positional token.
        let mentions_url = redacted_command_or_url(
            "docker run -e TOKEN=sk-live-abc img --url https://mcp.example.com",
        );
        assert_eq!(mentions_url, "docker …");
        assert!(
            !mentions_url.contains("sk-live-abc"),
            "argv secret reached the wire: {mentions_url}"
        );
    }

    use super::*;
    use crate::error_taxonomy::{ErrorCategory, ErrorEnvelope, ErrorSeverity};
    use crate::tools::goal::GoalStatus;
    use crate::tools::spec::ToolResult;
    use serde_json::json;

    const SOURCE: &str = include_str!("protocol_parity.rs");

    fn ids() -> ProtocolIds {
        ProtocolIds {
            thread_id: ThreadId::new(),
            session_id: SessionId::new(),
        }
    }

    #[test]
    fn worker_lifecycle_wire_preserves_typed_outcomes_without_parsing_result_text() {
        use crate::tools::subagent::SubAgentStatus;
        let ids = ids();
        for (outcome, expected) in [
            (Some(SubAgentStatus::Completed), Some("completed")),
            (
                Some(SubAgentStatus::Failed("private failure".into())),
                Some("failed"),
            ),
            (
                Some(SubAgentStatus::Interrupted("private reason".into())),
                Some("interrupted"),
            ),
            (Some(SubAgentStatus::Cancelled), Some("cancelled")),
            (
                Some(SubAgentStatus::BudgetExhausted),
                Some("budget_exhausted"),
            ),
            (None, None),
        ] {
            let event = Event::AgentComplete {
                display_name: None,
                owner_session_id: "owner".into(),
                id: "worker".into(),
                result: "Completed successfully".into(),
                outcome,
                parent_run_id: Some("parent".into()),
                spawn_depth: Some(2),
                continuable: Some(false),
                usage: None,
            };
            let wire = serde_json::to_value(event_to_protocol(&event, &ids)).unwrap();
            assert_eq!(wire["worker_status"].as_str(), expected);
            assert_eq!(wire["parent_run_id"], "parent");
            assert_eq!(wire["spawn_depth"], 2);
            assert_eq!(wire["continuable"], false);
            assert!(!wire.to_string().contains("private"));
        }
        for status in [
            AgentWorkerStatus::Queued,
            AgentWorkerStatus::Starting,
            AgentWorkerStatus::Running,
            AgentWorkerStatus::WaitingForUser,
            AgentWorkerStatus::ModelWait,
            AgentWorkerStatus::RunningTool,
            AgentWorkerStatus::Completed,
            AgentWorkerStatus::Failed,
            AgentWorkerStatus::Cancelled,
            AgentWorkerStatus::Interrupted,
        ] {
            // Runtime serializes the producer enum; stream-json uses this
            // exhaustive adapter. Their discriminants must remain identical.
            assert_eq!(
                serde_json::to_value(status).unwrap(),
                worker_status_str(status)
            );
        }
    }

    #[test]
    fn wire_accounting_preserves_parent_total_and_distinct_routed_telemetry() {
        let ids = ids();
        let total = Usage {
            input_tokens: 49,
            output_tokens: 19,
            ..Usage::default()
        };
        let parent = Usage {
            input_tokens: 7,
            output_tokens: 5,
            ..Usage::default()
        };
        let complete = event_to_protocol(
            &Event::TurnComplete {
                turn_id: None,
                submission_id: None,
                compaction_id: None,
                usage: total.clone(),
                parent_route_usage: parent.clone(),
                routed_usage_dropped_records: 3,
                status: TurnOutcomeStatus::Completed,
                error: None,
                tool_catalog: None,
                base_url: None,
            },
            &ids,
        );
        let json = serde_json::to_value(&complete).unwrap();
        assert_eq!(json["usage"]["input_tokens"], 49);
        assert_eq!(json["usage"]["output_tokens"], 19);
        assert_eq!(json["parent_route_usage"]["input_tokens"], 7);
        assert_eq!(json["parent_route_usage"]["output_tokens"], 5);
        assert_eq!(json["routed_usage_dropped_records"], 3);
        assert_eq!(
            serde_json::from_value::<wire::EventMsg>(json.clone()).unwrap(),
            complete
        );

        // A legacy terminal receipt has no parent subset, which differs from
        // an explicitly reported zero parent on a compaction-only turn.
        let mut legacy = json;
        legacy.as_object_mut().unwrap().remove("parent_route_usage");
        legacy
            .as_object_mut()
            .unwrap()
            .remove("routed_usage_dropped_records");
        assert!(matches!(
            serde_json::from_value::<wire::EventMsg>(legacy).unwrap(),
            wire::EventMsg::TurnComplete {
                parent_route_usage: None,
                routed_usage_dropped_records: 0,
                ..
            }
        ));

        for (event, tag) in [
            (
                Event::TurnUsage {
                    max_output_tokens: None,
                    usage: parent,
                    duration_ms: 12,
                    first_token_ms: Some(2),
                    request_ms: Some(10),
                    finish_reason: None,
                },
                "turn_usage",
            ),
            (
                Event::RoutedTurnUsage {
                    usage: total,
                    duration_ms: 27,
                    first_token_ms: None,
                    request_ms: None,
                },
                "routed_turn_usage",
            ),
        ] {
            let projected = event_to_protocol(&event, &ids);
            let json = serde_json::to_value(&projected).unwrap();
            assert_eq!(json["event"], tag);
            assert_eq!(
                serde_json::from_value::<wire::EventMsg>(json).unwrap(),
                projected
            );
        }
    }

    #[test]
    fn manual_compaction_terminal_keeps_its_operation_id_in_projection() {
        let event = Event::TurnComplete {
            turn_id: None,
            submission_id: None,
            compaction_id: Some("compact-1".into()),
            usage: Usage::default(),
            parent_route_usage: Usage::default(),
            routed_usage_dropped_records: 0,
            status: TurnOutcomeStatus::Failed,
            error: Some("route rejected".into()),
            tool_catalog: None,
            base_url: None,
        };
        let value = serde_json::to_value(event.to_protocol(&ids())).unwrap();
        assert_eq!(value["compaction_id"], "compact-1");
        assert!(value.get("turn_id").is_none());
        assert!(value.get("submission_id").is_none());
    }

    /// The guard is the exhaustive `match` in `event_to_protocol`: this test
    /// exists so the guard has a name in the test log and so the projection
    /// is proven to agree with the protocol's wire-tag table.
    #[test]
    fn protocol_covers_engine_events() {
        let ids = ids();
        let usage = Usage {
            input_tokens: 3,
            output_tokens: 4,
            ..Usage::default()
        };
        let events = vec![
            Event::MessageStarted { index: 0 },
            Event::MessageDelta {
                index: 0,
                content: "hello".into(),
            },
            Event::ThinkingDelta {
                index: 1,
                content: "hmm".into(),
            },
            Event::ToolCallStarted {
                model_call: None,
                id: "c1".into(),
                name: "read_file".into(),
                input: json!({"path": "x"}),
            },
            Event::ToolCallHeartbeat,
            Event::ToolCallComplete {
                model_call: None,
                id: "c1".into(),
                name: "read_file".into(),
                result: Ok(ToolResult::success("ok")),
            },
            Event::ToolCallComplete {
                model_call: None,
                id: "c2".into(),
                name: "bash".into(),
                result: Err(ToolError::Timeout { seconds: 9 }),
            },
            Event::TurnStarted {
                turn_id: "turn-1".into(),
                created_at: chrono::Utc::now(),
                route: None,
                // A host-stamped token, not `None`: the projection must carry
                // it verbatim or the assertion on `events[7]` below fails.
                submission_id: Some("sub-host-1".into()),
            },
            Event::TurnComplete {
                turn_id: Some("turn-1".into()),
                submission_id: Some("sub-host-1".into()),
                compaction_id: None,
                usage: usage.clone(),
                parent_route_usage: usage.clone(),
                routed_usage_dropped_records: 0,
                status: TurnOutcomeStatus::Interrupted,
                error: Some("stopped".into()),
                tool_catalog: None,
                base_url: Some("https://example.invalid".into()),
            },
            Event::WorkspaceSnapshotTaken {
                snapshot: codewhale_snapshot::WorkspaceSnapshotRef {
                    kind: codewhale_snapshot::WorkspaceSnapshotKind::Tool,
                    snapshot_id: "a".repeat(40),
                    tree_id: "b".repeat(40),
                    session_id: "thr_1".into(),
                    tool_call_id: Some("c1".into()),
                    write_paths: Some(vec!["src/lib.rs".into()]),
                    changed_paths: Some(Vec::new()),
                },
            },
            Event::RoutedTurnUsage {
                usage: usage.clone(),
                duration_ms: 12,
                first_token_ms: Some(3),
                request_ms: None,
            },
            Event::TurnUsage {
                max_output_tokens: None,
                usage,
                duration_ms: 12,
                first_token_ms: Some(3),
                request_ms: None,
                finish_reason: None,
            },
            Event::Error {
                envelope: ErrorEnvelope {
                    category: ErrorCategory::RateLimit,
                    severity: ErrorSeverity::Warning,
                    recoverable: true,
                    code: "E429".into(),
                    message: "slow down".into(),
                },
                recoverable: true,
            },
            Event::status("ready"),
            Event::PauseEvents { ack: None },
            Event::ResumeEvents,
            Event::ToolGateDecision {
                agent_id: None,
                tool_id: "c3".into(),
                tool_name: "bash".into(),
                gate: ToolGate::AutoReviewGuardian,
                decision: ToolGateVerdict::Unavailable,
                risk: None,
                reason: "timeout".into(),
            },
            Event::WorkflowUi {
                owner_session_id: "owner".into(),
                run_id: "run".into(),
                event: json!({"type": "task_started"}),
            },
        ];

        for event in &events {
            let msg = event.to_protocol(&ids);
            assert!(
                wire::EVENT_KINDS.contains(&msg.kind_str()),
                "{} is not in EVENT_KINDS",
                msg.kind_str()
            );
            assert_eq!(msg.thread_id(), &ids.thread_id);
            assert_eq!(msg.session_id(), &ids.session_id);
            let value = serde_json::to_value(&msg).unwrap();
            assert_eq!(value["event"], msg.kind_str());
            let back: wire::EventMsg = serde_json::from_value(value).unwrap();
            assert_eq!(back, msg);
        }

        let delta = events[1].to_protocol(&ids);
        let thinking = events[2].to_protocol(&ids);
        assert!(matches!(
            delta,
            wire::EventMsg::ResponseDelta {
                channel: ResponseChannel::Text,
                ..
            }
        ));
        assert!(matches!(
            thinking,
            wire::EventMsg::ResponseDelta {
                channel: ResponseChannel::Reasoning,
                ..
            }
        ));
        assert_eq!(
            serde_json::to_value(events[6].to_protocol(&ids)).unwrap()["result"],
            json!({"outcome": "err", "error": {"kind": "timeout", "seconds": 9}})
        );
        // The host correlation token must cross the projection verbatim: the
        // app forwarder binds its submit-window actions to this echo, so a
        // silently dropped mapping would defeat the contract.
        assert_eq!(
            serde_json::to_value(events[7].to_protocol(&ids)).unwrap()["submission_id"],
            json!("sub-host-1")
        );
        assert_eq!(
            serde_json::to_value(events[8].to_protocol(&ids)).unwrap()["status"],
            "interrupted"
        );
        let terminal = serde_json::to_value(events[8].to_protocol(&ids)).unwrap();
        assert_eq!(terminal["turn_id"], "turn-1");
        assert_eq!(terminal["submission_id"], "sub-host-1");
        assert!(terminal.get("compaction_id").is_none());
        let error = events
            .iter()
            .find(|event| matches!(event, Event::Error { .. }))
            .unwrap();
        let error = serde_json::to_value(error.to_protocol(&ids)).unwrap();
        assert_eq!(error["category"], "rate_limit");
        assert_eq!(error["severity"], "warning");
    }

    #[test]
    fn conditional_rewind_projection_preserves_expected_state_and_reply_owner() {
        let expected = crate::core::ops::SessionSnapshot {
            session_id: "observed-conversation".into(),
            messages: vec![],
            total_tokens: 9,
            model: "observed-model".into(),
            model_provider: "deepseek".into(),
            model_provider_id: Some("configured-provider".into()),
            workspace: std::path::PathBuf::from("/observed/workspace"),
            system_prompt: None,
            instruction_assembly: None,
            mode: "agent".into(),
        };
        let (tx, mut receive) = tokio::sync::oneshot::channel();
        let op = Op::RewindConversation {
            expected: Box::new(expected.clone()),
            messages: vec![],
            tx,
        };
        let value = serde_json::to_value(op.to_protocol()).unwrap();
        assert_eq!(value["kind"], "rewind_conversation");
        assert!(value["expected"] == to_value(&expected));
        assert!(value.get("tx").is_none());
        assert!(matches!(
            receive.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Empty)
        ));
    }

    #[test]
    fn protocol_covers_engine_ops() {
        let (tx, _rx) = tokio::sync::oneshot::channel();
        let settlement_reply = std::sync::Arc::new(std::sync::Mutex::new(Some(tx)));
        let ops = vec![
            Op::SetGoalStatus {
                goal_id: None,
                status: GoalStatus::Paused,
                clear: false,
            },
            Op::SetGoalObjective {
                goal_id: None,
                objective: "ship".into(),
                token_budget: Some(7),
            },
            Op::ListSubAgents,
            Op::CancelSubAgent {
                agent_id: "a1".into(),
            },
            Op::FollowUpSubAgent {
                agent_id: "a1".into(),
                text: "go".into(),
            },
            Op::SetStreamChunkTimeout { timeout_secs: 30 },
            Op::SetSubagentRuntimeConfig {
                enabled: true,
                max_subagents: 4,
                launch_concurrency: 2,
                max_spawn_depth: 1,
                api_timeout_secs: 60,
                heartbeat_timeout_secs: 10,
            },
            Op::CancelCompaction { id: "cmp".into() },
            Op::GetSessionSnapshot {
                tx: std::sync::Arc::new(std::sync::Mutex::new(None)),
            },
            Op::GetContextBudget {
                tx: std::sync::Arc::new(std::sync::Mutex::new(None)),
            },
            Op::GetProviderRuntimeStatus {
                tx: std::sync::Arc::new(std::sync::Mutex::new(None)),
            },
            Op::BootstrapMcp {
                tx: std::sync::Arc::new(std::sync::Mutex::new(None)),
            },
            Op::RetryMcpServer {
                name: "fs".into(),
                tx: std::sync::Arc::new(std::sync::Mutex::new(None)),
            },
            Op::ReloadMcp {
                config_path: std::path::PathBuf::from("/tmp/mcp.json"),
                tx: std::sync::Arc::new(std::sync::Mutex::new(None)),
            },
            Op::PurgeContext,
            Op::EditLastTurn {
                new_message: "again".into(),
                submission_id: None,
            },
            Op::SetAdvisorEnabled { enabled: true },
            Op::GetSubAgentSettlement {
                tx: std::sync::Arc::clone(&settlement_reply),
            },
            Op::Shutdown,
        ];

        for op in &ops {
            let msg = op.to_protocol();
            assert!(
                wire_op::OP_KINDS.contains(&msg.kind_str()),
                "{} is not in OP_KINDS",
                msg.kind_str()
            );
            let value = serde_json::to_value(&msg).unwrap();
            assert_eq!(value["kind"], msg.kind_str());
            let back: wire_op::Op = serde_json::from_value(value).unwrap();
            assert_eq!(back, msg);
        }

        assert_eq!(
            serde_json::to_value(ops[0].to_protocol()).unwrap(),
            json!({"kind": "set_goal_status", "status": "paused", "clear": false})
        );
        assert_eq!(
            serde_json::to_value(ops[8].to_protocol()).unwrap(),
            json!({"kind": "get_session_snapshot"}),
            "reply channels must not leak onto the wire"
        );
        let settlement = ops
            .iter()
            .find(|op| matches!(op, Op::GetSubAgentSettlement { .. }))
            .unwrap();
        assert_eq!(
            serde_json::to_value(settlement.to_protocol()).unwrap(),
            json!({"kind": "get_sub_agent_settlement"}),
            "the settlement operation must retain its own channel-free protocol twin"
        );
        assert!(
            settlement_reply.lock().unwrap().is_some(),
            "projection must not consume the host's live response sender"
        );
    }

    #[test]
    fn mode_labels_round_trip_through_app_mode_parse() {
        for mode in [AppMode::Agent, AppMode::Plan, AppMode::Operate] {
            assert_eq!(AppMode::parse(app_mode_str(mode)), Some(mode), "{mode:?}");
        }
        for mode in [
            ApprovalMode::Auto,
            ApprovalMode::Bypass,
            ApprovalMode::Suggest,
            ApprovalMode::Never,
        ] {
            assert_eq!(
                ApprovalMode::from_config_value(approval_mode_str(mode)),
                Some(mode)
            );
        }
    }

    /// The projections are only a guard while they stay exhaustive. A
    /// wildcard arm would let a new engine variant slip through unmapped.
    #[test]
    fn projections_have_no_wildcard_arms() {
        let wildcard_arms: Vec<&str> = SOURCE
            .lines()
            .filter(|line| {
                let trimmed = line.trim_start();
                trimmed.starts_with("_ =>")
                    || trimmed.starts_with("_=>")
                    || trimmed.starts_with("Event::_")
                    || trimmed.starts_with("Op::_")
                    || (trimmed.contains(" => ") && trimmed.starts_with("other =>"))
            })
            .collect();
        assert!(
            wildcard_arms.is_empty(),
            "protocol_parity.rs must match engine variants exhaustively; found {wildcard_arms:?}"
        );
    }
    #[test]
    fn acp_optional_tool_facts_round_trip_with_core_routing_identity() {
        let ids = ids();
        for event in [
            Event::ToolExecutionStarted {
                id: "core-call".into(),
            },
            Event::ToolResultContent {
                id: "core-call".into(),
                blocks: vec![codewhale_tools::ToolResultContentBlock::Image {
                    mime_type: "image/png".into(),
                    data: "QUJD".into(),
                }],
            },
        ] {
            let projected = event_to_protocol(&event, &ids);
            assert_eq!(projected.thread_id(), &ids.thread_id);
            assert_eq!(projected.session_id(), &ids.session_id);
            let value = serde_json::to_value(&projected).unwrap();
            assert_eq!(
                serde_json::from_value::<wire::EventMsg>(value).unwrap(),
                projected
            );
        }
    }
}

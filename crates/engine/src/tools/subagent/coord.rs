//! Narrow model-facing agent coordination tools.
//!
//! Keeps `agent` as the creation surface. These five tools wrap existing
//! SubAgentManager / mailbox / checkpoint machinery without restoring the
//! retired lifecycle theater (`agent_open` / `agent_eval` / …).

use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde_json::{Value, json};

use super::{
    COMPLETED_AGENT_RETENTION, NEEDS_PERSON_WAIT_NOTE, ParentMailReceipt, SharedSubAgentManager,
    SubAgentRuntime, SubAgentStatus, parse_agent_ref, subagent_session_projection,
    subagent_status_name, take_new_needs_person, wait_for_subagents_from_input,
};
use crate::tools::registry::ToolRegistryBuilder;
use crate::tools::spec::{
    ApprovalRequirement, ToolCapability, ToolContext, ToolError, ToolResult, ToolSpec,
};

/// Bounds for `agents/wait`. Short on purpose: a blocked wait makes the
/// session deaf to typed input, and settled children already report back as
/// `<codewhale:subagent.done>` sentinels that start a fresh turn (#4097).
/// `pub` so the description-pinning test can tie the advertised
/// numbers to the runtime constants.
pub const COORD_WAIT_DEFAULT_TIMEOUT_SECS: u64 = 30;
const COORD_WAIT_MIN_TIMEOUT_SECS: u64 = 1;
pub const COORD_WAIT_MAX_TIMEOUT_SECS: u64 = 120;
const COORD_WAIT_CHECK_INTERVAL: Duration = Duration::from_millis(250);
const RECENT_PROGRESS_LIMIT: usize = 8;
pub(super) const COORDINATION_RECORD_LIMIT: usize = 128;
const COORDINATION_INSPECT_LIMIT: usize = 24;
pub(super) const COORDINATION_PROJECTION_DECISION_LIMIT: usize = 8;
pub(super) const COORDINATION_PROJECTION_BYTE_LIMIT: usize = 4096;

mod ledger;

// The ledger types moved to `ledger` unchanged and are re-published here, so
// `crate::tools::subagent::coord::{DecisionRecord, …}` still resolves for every
// consumer that never had reason to know where the definitions sit — the point
// of the split was to shorten two files, not to make four other files import
// differently.
//
// A glob, not a list: several of these types are named only from `cfg(test)`
// code in other modules, and an explicit `pub use` of those reads as an unused
// import in a release build. The glob also keeps each item's own visibility, so
// `MAX_RECONCILIATION_RETRIES` stays reachable here without becoming part of
// the module's public surface.
pub use ledger::*;

// ── agents/list ──────────────────────────────────────────────────────────

pub struct AgentsListTool {
    manager: SharedSubAgentManager,
}

impl AgentsListTool {
    #[must_use]
    pub fn new(manager: SharedSubAgentManager) -> Self {
        Self { manager }
    }
}

#[async_trait]
impl ToolSpec for AgentsListTool {
    fn model_visible(&self) -> bool {
        // #5462: `agent` is the sole model-facing sub-agent surface. These
        // narrow tools stay registered and executable by name so a persisted
        // transcript replays byte-for-byte, but they are never advertised in
        // the catalog and can never be returned by `tool_search` — the same
        // shape `rlm` and `exec_shell` already use.
        false
    }

    fn name(&self) -> &'static str {
        "agents/list"
    }

    fn description(&self) -> &'static str {
        "List child agents: ids, parent hierarchy, state, bounded recent progress, and token budget. Read-only coordination view — does not spawn or wake workers."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "include_archived": {
                    "type": "boolean",
                    "description": "Include prior-session / archived agents. Default false."
                },
                "agent_id": {
                    "type": "string",
                    "description": "Optional single agent id or session name to inspect."
                }
            },
            "required": []
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::ReadOnly]
    }

    fn approval_requirement(&self) -> ApprovalRequirement {
        ApprovalRequirement::Auto
    }

    fn is_read_only_for(&self, _input: &Value) -> bool {
        true
    }

    fn supports_parallel_for(&self, _input: &Value) -> bool {
        true
    }

    async fn execute(&self, input: Value, context: &ToolContext) -> Result<ToolResult, ToolError> {
        let include_archived = input
            .get("include_archived")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let agent_ref = parse_agent_ref(&input)?;

        let mut manager = self.manager.write().await;
        manager.cleanup_for_session(&context.state_namespace, COMPLETED_AGENT_RETENTION);
        let summaries = if let Some(agent_ref) = agent_ref {
            let summary = manager
                .coordination_summary_for_session(
                    &context.state_namespace,
                    &agent_ref,
                    RECENT_PROGRESS_LIMIT,
                )
                .map_err(|err| ToolError::invalid_input(err.to_string()))?;
            vec![summary]
        } else {
            manager.list_coordination_summaries_for_session(
                &context.state_namespace,
                include_archived,
                RECENT_PROGRESS_LIMIT,
            )
        };
        drop(manager);

        let payload = json!({
            "action": "list",
            "count": summaries.len(),
            "agents": summaries,
        });
        let mut tool_result = ToolResult::json(&payload)
            .map_err(|err| ToolError::execution_failed(err.to_string()))?;
        tool_result.metadata = Some(json!({
            "action": "list",
            "count": summaries.len(),
        }));
        Ok(tool_result)
    }
}

// ── agents/message ───────────────────────────────────────────────────────

pub struct AgentsMessageTool {
    manager: SharedSubAgentManager,
    caller_agent_id: Option<String>,
}

impl AgentsMessageTool {
    #[must_use]
    pub fn new(manager: SharedSubAgentManager) -> Self {
        Self {
            manager,
            caller_agent_id: None,
        }
    }

    #[must_use]
    pub fn with_optional_caller(mut self, caller_agent_id: Option<String>) -> Self {
        self.caller_agent_id = caller_agent_id;
        self
    }
}

#[async_trait]
impl ToolSpec for AgentsMessageTool {
    fn model_visible(&self) -> bool {
        // #5462: `agent` is the sole model-facing sub-agent surface. These
        // narrow tools stay registered and executable by name so a persisted
        // transcript replays byte-for-byte, but they are never advertised in
        // the catalog and can never be returned by `tool_search` — the same
        // shape `rlm` and `exec_shell` already use.
        false
    }

    fn name(&self) -> &'static str {
        "agents/message"
    }

    fn description(&self) -> &'static str {
        "Queue a parent message onto a running child without waking it. The message stays queued until a later agents/followup delivers it through the child's live input channel. Use agents/followup directly when you want immediate delivery."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "agent_id": {
                    "type": "string",
                    "description": "Target child agent id or session name."
                },
                "message": {
                    "type": "string",
                    "description": "Message text to queue."
                }
            },
            "required": ["agent_id", "message"]
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::RequiresApproval]
    }

    fn approval_requirement(&self) -> ApprovalRequirement {
        ApprovalRequirement::Required
    }

    async fn execute(&self, input: Value, context: &ToolContext) -> Result<ToolResult, ToolError> {
        let agent_ref =
            parse_agent_ref(&input)?.ok_or_else(|| ToolError::missing_field("agent_id"))?;
        let message = input
            .get("message")
            .or_else(|| input.get("text"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| ToolError::missing_field("message"))?
            .to_string();

        let receipt = {
            let mut manager = self.manager.write().await;
            manager
                .ensure_caller_controls_descendant_for_session(
                    &context.state_namespace,
                    &agent_ref,
                    self.caller_agent_id.as_deref(),
                    "agents/message",
                )
                .map_err(|err| ToolError::invalid_input(err.to_string()))?;
            manager
                .queue_running_parent_message_for_session(
                    &context.state_namespace,
                    &agent_ref,
                    message,
                )
                .map_err(|err| ToolError::invalid_input(err.to_string()))?
        };

        let payload = json!({
            "action": "message",
            "agent_id": receipt.agent_id,
            "queued": true,
            "woke": false,
            "queue_depth": receipt.queue_depth,
            "status": receipt.status,
            "note": "Message queued without waking the child.",
        });
        let mut tool_result = ToolResult::json(&payload)
            .map_err(|err| ToolError::execution_failed(err.to_string()))?;
        tool_result.metadata = Some(json!({
            "action": "message",
            "agent_id": receipt.agent_id,
            "woke": false,
            "queue_depth": receipt.queue_depth,
        }));
        Ok(tool_result)
    }
}

// ── agents/followup ──────────────────────────────────────────────────────

pub struct AgentsFollowupTool {
    manager: SharedSubAgentManager,
    caller_agent_id: Option<String>,
    /// Runtime for checkpoint resume. `None` (legacy/test construction)
    /// keeps the queue-only followup behavior.
    runtime: Option<SubAgentRuntime>,
}

impl AgentsFollowupTool {
    #[must_use]
    pub fn new(manager: SharedSubAgentManager) -> Self {
        Self {
            manager,
            caller_agent_id: None,
            runtime: None,
        }
    }

    #[must_use]
    pub fn with_runtime(mut self, runtime: SubAgentRuntime) -> Self {
        self.runtime = Some(runtime);
        self
    }

    #[must_use]
    pub fn with_optional_caller(mut self, caller_agent_id: Option<String>) -> Self {
        self.caller_agent_id = caller_agent_id;
        self
    }
}

impl AgentsFollowupTool {
    async fn followup_one(
        &self,
        agent_ref: &str,
        message: &str,
        context: &ToolContext,
    ) -> Result<Value, ToolError> {
        let mut manager = self.manager.write().await;
        let (source, target) = manager
            .continuation_target_for_caller(
                &context.state_namespace,
                agent_ref,
                self.caller_agent_id.as_deref(),
                "agents/followup",
            )
            .map_err(|error| ToolError::invalid_input(error.to_string()))?;
        let snapshot = manager
            .get_result(&target)
            .map_err(|error| ToolError::invalid_input(error.to_string()))?;
        let resumed_already = source != target;
        let receipt = if super::subagent_checkpoint_is_continuable(&snapshot)
            && self.runtime.is_some()
        {
            let snapshot = manager
                .resume_from_checkpoint_for_session(
                    &context.state_namespace,
                    Arc::clone(&self.manager),
                    self.runtime.clone().expect("runtime checked"),
                    &target,
                    message,
                )
                .map_err(|error| ToolError::execution_failed(error.to_string()))?;
            ParentMailReceipt {
                agent_id: snapshot.agent_id.clone(),
                status: subagent_status_name(&snapshot.status).to_string(),
                queue_depth: 0,
                woke: true,
                continued_from_checkpoint: true,
                continuation_handle: None,
                note: format!(
                    "resumed from checkpoint {source} as {}; original receipt retained",
                    snapshot.agent_id
                ),
            }
        } else if resumed_already && snapshot.status != SubAgentStatus::Running {
            ParentMailReceipt {
                agent_id: target, status: subagent_status_name(&snapshot.status).to_string(),
                queue_depth: 0, woke: false, continued_from_checkpoint: true,
                continuation_handle: None,
                note: "Existing continuation has settled; no duplicate worker was started and no message was delivered.".to_string(),
            }
        } else {
            manager
                .followup_child_for_session(&context.state_namespace, &target, message.to_string())
                .map_err(|error| ToolError::invalid_input(error.to_string()))?
        };
        let child_route = manager
            .get_worker_record_for_session(&context.state_namespace, &receipt.agent_id)
            .and_then(|record| record.spec.child_route);
        Ok(json!({
            "action": "followup", "from": source, "to": receipt.agent_id,
            "agent_id": receipt.agent_id, "queued": receipt.woke || receipt.queue_depth > 0,
            "woke": receipt.woke, "queue_depth": receipt.queue_depth, "status": receipt.status,
            "continued_from_checkpoint": receipt.continued_from_checkpoint || resumed_already,
            "continuation_handle": receipt.continuation_handle, "note": receipt.note,
            "child_route": child_route,
        }))
    }
}

#[async_trait]
impl ToolSpec for AgentsFollowupTool {
    fn model_visible(&self) -> bool {
        // #5462: `agent` is the sole model-facing sub-agent surface. These
        // narrow tools stay registered and executable by name so a persisted
        // transcript replays byte-for-byte, but they are never advertised in
        // the catalog and can never be returned by `tool_search` — the same
        // shape `rlm` and `exec_shell` already use.
        false
    }

    fn name(&self) -> &'static str {
        "agents/followup"
    }

    fn description(&self) -> &'static str {
        "Queue a message and attempt to resume an idle or interrupted child. Running children receive the message on their next step; interrupted_continuable children are resumed from their checkpoint into a fresh agent loop (new agent id, original prompt plus prior conversation tail) when a runtime is attached, and otherwise keep queue-only semantics with the continuation_handle returned."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "agent_id": {"type": "string"},
                "agent_ids": {"type": "array", "items": {"type": "string", "minLength": 1}, "minItems": 1, "maxItems": 32},
                "all_parked": {"type": "boolean", "description": "Continue every owned parked child, up to 32."},
                "message": {"type": "string", "minLength": 1}
            },
            "required": ["message"],
            "oneOf": [{"required": ["agent_id"]}, {"required": ["agent_ids"]}, {"required": ["all_parked"]}]
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::RequiresApproval]
    }

    fn approval_requirement(&self) -> ApprovalRequirement {
        ApprovalRequirement::Required
    }

    async fn execute(&self, input: Value, context: &ToolContext) -> Result<ToolResult, ToolError> {
        let message = input
            .get("message")
            .or_else(|| input.get("text"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| ToolError::missing_field("message"))?;
        let single = parse_agent_ref(&input)?;
        let all_parked = super::parse_optional_bool(&input, &["all_parked"])?.unwrap_or(false);
        let batch = input.get("agent_ids");
        if usize::from(single.is_some()) + usize::from(batch.is_some()) + usize::from(all_parked)
            != 1
        {
            return Err(ToolError::invalid_input(
                "followup requires exactly one of agent_id, agent_ids, or all_parked=true",
            ));
        }
        let mut targets = if let Some(ids) = batch {
            let ids = ids
                .as_array()
                .filter(|ids| !ids.is_empty() && ids.len() <= 32)
                .ok_or_else(|| {
                    ToolError::invalid_input("agent_ids must contain 1..32 nonempty strings")
                })?;
            ids.iter()
                .map(|id| {
                    id.as_str()
                        .map(str::trim)
                        .filter(|id| !id.is_empty())
                        .map(str::to_string)
                        .ok_or_else(|| {
                            ToolError::invalid_input("agent_ids must contain nonempty strings")
                        })
                })
                .collect::<Result<Vec<_>, _>>()?
        } else if let Some(id) = single.as_ref() {
            vec![id.clone()]
        } else {
            let manager = self.manager.read().await;
            let mut ids = manager
                .agents
                .values()
                .filter(|agent| {
                    manager.agent_is_owned_by_session(agent, &context.state_namespace)
                        && agent
                            .checkpoint
                            .as_ref()
                            .is_some_and(|checkpoint| checkpoint.parked_at_turn_end)
                        && matches!(agent.status, SubAgentStatus::Interrupted(_))
                        && manager
                            .ensure_caller_controls_descendant(
                                &agent.id,
                                self.caller_agent_id.as_deref(),
                                "agents/followup",
                            )
                            .is_ok()
                        && manager
                            .continuation_target(&agent.id)
                            .is_ok_and(|target| target == agent.id)
                })
                .map(|agent| agent.id.clone())
                .collect::<Vec<_>>();
            ids.sort();
            if ids.len() > 32 {
                return Err(ToolError::invalid_input(
                    "More than 32 parked children; use explicit agent_ids batches",
                ));
            }
            ids
        };
        let mut seen = std::collections::HashSet::new();
        targets.retain(|target| seen.insert(target.clone()));
        let mut results = Vec::new();
        let mut errors = Vec::new();
        // Each mutation and both hierarchy checks share the manager write lock.
        // A target failure cannot erase successful results from another target.
        for target in targets {
            match self.followup_one(&target, message, context).await {
                Ok(payload) => results.push(payload),
                Err(error) if single.is_some() => return Err(error),
                Err(error) => errors.push(json!({"from": target, "error": error.to_string()})),
            }
        }
        let payload = if single.is_some() {
            results.pop().expect("single target returned a result")
        } else {
            json!({"action": "followup", "results": results, "errors": errors})
        };
        let mut result = ToolResult::json(&payload)
            .map_err(|error| ToolError::execution_failed(error.to_string()))?;
        result.metadata = Some(payload.clone());
        Ok(result)
    }
}

// ── agents/interrupt ─────────────────────────────────────────────────────

pub struct AgentsInterruptTool {
    manager: SharedSubAgentManager,
    /// Optional caller identity for fail-closed self-interrupt checks.
    caller_agent_id: Option<String>,
}

impl AgentsInterruptTool {
    #[must_use]
    pub fn new(manager: SharedSubAgentManager) -> Self {
        Self {
            manager,
            caller_agent_id: None,
        }
    }

    #[must_use]
    #[allow(dead_code)] // arms self-interrupt fail-closed when child registries thread caller (P1.2)
    pub fn with_caller(mut self, caller_agent_id: impl Into<String>) -> Self {
        self.caller_agent_id = Some(caller_agent_id.into());
        self
    }

    #[must_use]
    pub fn with_optional_caller(mut self, caller_agent_id: Option<String>) -> Self {
        self.caller_agent_id = caller_agent_id;
        self
    }
}

#[async_trait]
impl ToolSpec for AgentsInterruptTool {
    fn model_visible(&self) -> bool {
        // #5462: `agent` is the sole model-facing sub-agent surface. These
        // narrow tools stay registered and executable by name so a persisted
        // transcript replays byte-for-byte, but they are never advertised in
        // the catalog and can never be returned by `tool_search` — the same
        // shape `rlm` and `exec_shell` already use.
        false
    }

    fn name(&self) -> &'static str {
        "agents/interrupt"
    }

    fn description(&self) -> &'static str {
        "Interrupt a running child agent, preserve its checkpoint, and return the prior state. Fails closed on root or self targets. Prefer this over cancel when you may resume later."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "agent_id": {
                    "type": "string",
                    "description": "Child agent id or session name to interrupt."
                },
                "reason": {
                    "type": "string",
                    "description": "Optional interrupt reason recorded on the checkpoint."
                }
            },
            "required": ["agent_id"]
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::RequiresApproval]
    }

    fn approval_requirement(&self) -> ApprovalRequirement {
        ApprovalRequirement::Required
    }

    async fn execute(&self, input: Value, context: &ToolContext) -> Result<ToolResult, ToolError> {
        let agent_ref =
            parse_agent_ref(&input)?.ok_or_else(|| ToolError::missing_field("agent_id"))?;
        let reason = input
            .get("reason")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("interrupted by parent via agents/interrupt")
            .to_string();

        let (prior, snapshot) = {
            let mut manager = self.manager.write().await;
            manager
                .interrupt_child_for_session(
                    &context.state_namespace,
                    &agent_ref,
                    self.caller_agent_id.as_deref(),
                    reason,
                )
                .map_err(|err| ToolError::invalid_input(err.to_string()))?
        };

        let snapshot = super::settle_requested_child(&self.manager, snapshot).await;
        let worker_record = {
            let manager = self.manager.read().await;
            manager.get_worker_record_for_session(&context.state_namespace, &snapshot.agent_id)
        };
        let projection =
            subagent_session_projection(&self.manager, snapshot, false, context, worker_record)
                .await;
        let payload = json!({
            "action": "interrupt",
            "agent_id": projection.agent_id,
            "prior_status": subagent_status_name(&prior.status),
            "prior_steps_taken": prior.steps_taken,
            "status": projection.status,
            "checkpoint_preserved": projection.checkpoint.is_some(),
            "continuable": projection.continuable,
            "projection": projection,
            "child_route": projection.child_route,
        });
        let mut tool_result = ToolResult::json(&payload)
            .map_err(|err| ToolError::execution_failed(err.to_string()))?;
        tool_result.metadata = Some(json!({
            "action": "interrupt",
            "agent_id": payload["agent_id"],
            "checkpoint_preserved": payload["checkpoint_preserved"],
            "child_route": payload["child_route"],
        }));
        Ok(tool_result)
    }
}

// ── agents/wait ──────────────────────────────────────────────────────────

pub struct AgentsWaitTool {
    manager: SharedSubAgentManager,
}

impl AgentsWaitTool {
    #[must_use]
    pub fn new(manager: SharedSubAgentManager) -> Self {
        Self { manager }
    }
}

#[async_trait]
impl ToolSpec for AgentsWaitTool {
    fn model_visible(&self) -> bool {
        // #5462: `agent` is the sole model-facing sub-agent surface. These
        // narrow tools stay registered and executable by name so a persisted
        // transcript replays byte-for-byte, but they are never advertised in
        // the catalog and can never be returned by `tool_search` — the same
        // shape `rlm` and `exec_shell` already use.
        false
    }

    fn name(&self) -> &'static str {
        "agents/wait"
    }

    fn description(&self) -> &'static str {
        "Block briefly until one child settles or timeout_secs (default 30, max 120) elapses; on timeout the receipt reports timed_out=true and any settled children. Keep waits short: on timeout, end your turn — settled children wake you automatically as completion sentinels; polling agents/list in a loop is not the right shape either. until=all is the fan-out join: it returns only when every child running at call time has left running, with each child's outcome. until=completion (default) returns as soon as any one child settles. until=activity also returns on progress."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "agent_id": {
                    "type": "string",
                    "description": "Optional specific child. When omitted, watches every child running at call time."
                },
                "timeout_secs": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 120,
                    "description": "Maximum seconds to block. Default 30. Keep it short — on timeout, end your turn; settled children report back as completion sentinels."
                },
                "until": {
                    "type": "string",
                    "enum": ["completion", "all", "activity"],
                    "description": "completion (default): return when any one child leaves running. all: return only when every watched child has left running — use this after a fan-out so one wait covers the whole batch. activity: also return when recent progress changes. Children spawned after the call are not watched; no children means an immediate return."
                }
            },
            "required": []
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::ReadOnly]
    }

    fn approval_requirement(&self) -> ApprovalRequirement {
        ApprovalRequirement::Auto
    }

    fn is_read_only_for(&self, _input: &Value) -> bool {
        true
    }

    async fn execute(&self, input: Value, context: &ToolContext) -> Result<ToolResult, ToolError> {
        dispatch_wait(&input, Arc::clone(&self.manager), context).await
    }
}

/// Single entry point for every blocking wait, shared by `agents/wait` and
/// `agent(action="wait")` so the two surfaces cannot drift.
///
/// `until` selects the join shape:
/// - `completion` (default) — return as soon as any one watched child settles.
/// - `all` — return only when every watched child has settled (the fan-out
///   join the parent should use after dispatching a batch).
/// - `activity` — also return when a running child makes visible progress.
pub(super) async fn dispatch_wait(
    input: &Value,
    manager: SharedSubAgentManager,
    context: &ToolContext,
) -> Result<ToolResult, ToolError> {
    let until = input
        .get("until")
        .and_then(Value::as_str)
        .unwrap_or("completion")
        .trim()
        .to_ascii_lowercase();

    match until.as_str() {
        "" | "completion" => {
            let mut wait_input = input.clone();
            if wait_input.get("action").is_none() {
                wait_input["action"] = json!("wait");
            }
            wait_for_subagents_from_input(&wait_input, manager, context).await
        }
        "all" => wait_for_all_children(input, manager, context).await,
        "activity" => wait_for_activity(input, manager, context).await,
        other => Err(ToolError::invalid_input(format!(
            "Invalid until '{other}'. Use completion, all, or activity."
        ))),
    }
}

/// `until=all`: block until every child that was running when the call was
/// made has left `Running`.
///
/// The watch set is fixed at call time. A child spawned while this wait is
/// blocked is deliberately **not** joined — the parent asked to join the batch
/// it had just dispatched, and silently extending the set would make the call
/// unbounded in a way the caller never asked for. Callers that fan out again
/// simply issue another wait.
///
/// Cancel-safe (no lock is held across an await), honours `timeout_secs`, and
/// returns immediately with `all_settled: true` when nothing is running.
async fn wait_for_all_children(
    input: &Value,
    manager: SharedSubAgentManager,
    context: &ToolContext,
) -> Result<ToolResult, ToolError> {
    let timeout_secs = input
        .get("timeout_secs")
        .or_else(|| input.get("timeout"))
        .and_then(Value::as_u64)
        .unwrap_or(COORD_WAIT_DEFAULT_TIMEOUT_SECS)
        .clamp(COORD_WAIT_MIN_TIMEOUT_SECS, COORD_WAIT_MAX_TIMEOUT_SECS);
    let timeout = Duration::from_secs(timeout_secs);
    let agent_ref = parse_agent_ref(input)?;

    // Resolve the watch set up front so a bad reference fails immediately
    // rather than blocking for the whole timeout.
    let watched: Vec<String> = {
        let manager = manager.read().await;
        if let Some(agent_ref) = &agent_ref {
            let snapshot = manager
                .get_result_by_ref_for_session(&context.state_namespace, agent_ref)
                .map_err(|err| ToolError::invalid_input(err.to_string()))?;
            if snapshot.status != SubAgentStatus::Running {
                // Already settled: hand back its outcome rather than an empty
                // "nothing to join" that hides what the caller asked about.
                let settled = json!({
                    "agent_id": snapshot.agent_id,
                    "name": snapshot.name,
                    "status": subagent_status_name(&snapshot.status),
                    "steps_taken": snapshot.steps_taken,
                });
                drop(manager);
                return wait_all_payload(&[settled], &[], &[], 0, false);
            }
            vec![snapshot.agent_id]
        } else {
            manager
                .list_filtered_for_session(&context.state_namespace, false)
                .into_iter()
                .filter(|snapshot| snapshot.status == SubAgentStatus::Running)
                .map(|snapshot| snapshot.agent_id)
                .collect()
        }
    };

    // Zero children is an immediate return, never a hang.
    if watched.is_empty() {
        return wait_all_payload(&[], &[], &[], 0, false);
    }

    let started = Instant::now();
    let cancelled = async {
        match &context.cancel_token {
            Some(token) => token.cancelled().await,
            None => std::future::pending().await,
        }
    };
    tokio::pin!(cancelled);

    loop {
        let (settled, still_running) = {
            let manager = manager.read().await;
            let mut settled = Vec::new();
            let mut still_running = Vec::new();
            for agent_id in &watched {
                match manager.get_result_by_ref_for_session(&context.state_namespace, agent_id) {
                    Ok(snapshot) if snapshot.status == SubAgentStatus::Running => {
                        still_running.push(json!({
                            "agent_id": snapshot.agent_id,
                            "name": snapshot.name,
                            "status": "running",
                        }));
                    }
                    Ok(snapshot) => settled.push(json!({
                        "agent_id": snapshot.agent_id,
                        "name": snapshot.name,
                        "status": subagent_status_name(&snapshot.status),
                        "steps_taken": snapshot.steps_taken,
                    })),
                    // A watched child that vanished from the ledger (retention
                    // cleanup) is no longer running; report it rather than
                    // blocking on a record that will never settle.
                    Err(_) => settled.push(json!({
                        "agent_id": agent_id,
                        "status": "gone",
                    })),
                }
            }
            (settled, still_running)
        };

        if still_running.is_empty() {
            return wait_all_payload(&settled, &[], &[], started.elapsed().as_millis(), false);
        }
        // A child blocked on a person ends the join early (approvals C2).
        let needs_person = take_new_needs_person(&manager, &watched).await;
        if !needs_person.is_empty() {
            return wait_all_payload(
                &settled,
                &still_running,
                &needs_person,
                started.elapsed().as_millis(),
                false,
            );
        }
        if started.elapsed() >= timeout {
            return wait_all_payload(
                &settled,
                &still_running,
                &[],
                started.elapsed().as_millis(),
                true,
            );
        }

        tokio::select! {
            biased;
            () = &mut cancelled => {
                return Err(ToolError::cancelled(
                    "Wait interrupted by user cancellation before every child settled.".to_string(),
                ));
            }
            () = tokio::time::sleep(COORD_WAIT_CHECK_INTERVAL) => {}
        }
    }
}

/// `until=all` result: every watched child with its own outcome, so the parent
/// can synthesize from one return instead of re-inspecting each child.
fn wait_all_payload(
    settled: &[Value],
    still_running: &[Value],
    needs_person: &[Value],
    waited_ms: u128,
    timed_out: bool,
) -> Result<ToolResult, ToolError> {
    let note = if !needs_person.is_empty() {
        NEEDS_PERSON_WAIT_NOTE
    } else if timed_out {
        "The wait interval ended; the children are still running. You may answer the user or continue other work. Ordinary turn completion keeps them running; results arrive as <codewhale:subagent.done> sentinels. Use followup only when a child actually needs continuation."
    } else if settled.is_empty() {
        "No sub-agents were running; nothing to join."
    } else {
        "Every watched child has settled. Full results arrive as <codewhale:subagent.done> sentinels — synthesize from those."
    };
    let mut payload = json!({
        "action": "wait",
        "until": "all",
        "all_settled": still_running.is_empty(),
        "settled": settled,
        "still_running": still_running,
        "waited_ms": u64::try_from(waited_ms).unwrap_or(u64::MAX),
        "timed_out": timed_out,
        "note": note,
    });
    if !needs_person.is_empty() {
        payload["needs_person"] = json!(needs_person);
    }
    let mut tool_result =
        ToolResult::json(&payload).map_err(|err| ToolError::execution_failed(err.to_string()))?;
    tool_result.metadata = Some(json!({
        "action": "wait",
        "until": "all",
        "all_settled": still_running.is_empty(),
        "settled": settled.len(),
        "running": still_running.len(),
        "timed_out": timed_out,
    }));
    Ok(tool_result)
}

async fn wait_for_activity(
    input: &Value,
    manager: SharedSubAgentManager,
    context: &ToolContext,
) -> Result<ToolResult, ToolError> {
    let timeout_secs = input
        .get("timeout_secs")
        .or_else(|| input.get("timeout"))
        .and_then(Value::as_u64)
        .unwrap_or(COORD_WAIT_DEFAULT_TIMEOUT_SECS)
        .clamp(COORD_WAIT_MIN_TIMEOUT_SECS, COORD_WAIT_MAX_TIMEOUT_SECS);
    let timeout = Duration::from_secs(timeout_secs);
    let agent_ref = parse_agent_ref(input)?;

    let (watched, baseline): (Vec<String>, Vec<(String, u64)>) = {
        let manager = manager.read().await;
        if let Some(agent_ref) = &agent_ref {
            let snap = manager
                .get_result_by_ref_for_session(&context.state_namespace, agent_ref)
                .map_err(|err| ToolError::invalid_input(err.to_string()))?;
            let fp = manager.activity_fingerprint(&snap.agent_id).unwrap_or(0);
            if snap.status != SubAgentStatus::Running {
                let payload = json!({
                    "action": "wait",
                    "until": "activity",
                    "reason": "already_settled",
                    "timed_out": false,
                    "agent_id": snap.agent_id,
                    "status": subagent_status_name(&snap.status),
                });
                let mut tool_result = ToolResult::json(&payload)
                    .map_err(|err| ToolError::execution_failed(err.to_string()))?;
                tool_result.metadata = Some(json!({ "action": "wait", "timed_out": false }));
                return Ok(tool_result);
            }
            (vec![snap.agent_id.clone()], vec![(snap.agent_id, fp)])
        } else {
            let running = manager
                .list_filtered_for_session(&context.state_namespace, false)
                .into_iter()
                .filter(|s| s.status == SubAgentStatus::Running)
                .map(|s| s.agent_id)
                .collect::<Vec<_>>();
            let baseline = running
                .iter()
                .map(|id| {
                    let fp = manager.activity_fingerprint(id).unwrap_or(0);
                    (id.clone(), fp)
                })
                .collect();
            (running, baseline)
        }
    };

    if watched.is_empty() {
        let payload = json!({
            "action": "wait",
            "until": "activity",
            "note": "No running sub-agents; nothing to wait for.",
            "timed_out": false,
        });
        let mut tool_result = ToolResult::json(&payload)
            .map_err(|err| ToolError::execution_failed(err.to_string()))?;
        tool_result.metadata = Some(json!({ "action": "wait", "timed_out": false }));
        return Ok(tool_result);
    }

    let started = Instant::now();
    let cancelled = async {
        match &context.cancel_token {
            Some(token) => token.cancelled().await,
            None => std::future::pending().await,
        }
    };
    tokio::pin!(cancelled);

    loop {
        let outcome = {
            let manager = manager.read().await;
            let mut settled = Vec::new();
            let mut activity = Vec::new();
            for (id, base_fp) in &baseline {
                if let Ok(snap) =
                    manager.get_result_by_ref_for_session(&context.state_namespace, id)
                {
                    if snap.status != SubAgentStatus::Running {
                        settled.push(snap);
                        continue;
                    }
                    let fp = manager.activity_fingerprint(id).unwrap_or(0);
                    if fp != *base_fp {
                        activity.push(json!({
                            "agent_id": id,
                            "status": "running",
                            "activity_fingerprint": fp,
                        }));
                    }
                }
            }
            (
                settled,
                activity,
                manager.running_count_for_session(&context.state_namespace),
            )
        };

        if !outcome.0.is_empty() || !outcome.1.is_empty() {
            let payload = json!({
                "action": "wait",
                "until": "activity",
                "settled": outcome.0.iter().map(|s| json!({
                    "agent_id": s.agent_id,
                    "status": subagent_status_name(&s.status),
                })).collect::<Vec<_>>(),
                "activity": outcome.1,
                "running": outcome.2,
                "elapsed_ms": started.elapsed().as_millis(),
                "timed_out": false,
            });
            let mut tool_result = ToolResult::json(&payload)
                .map_err(|err| ToolError::execution_failed(err.to_string()))?;
            tool_result.metadata = Some(json!({
                "action": "wait",
                "timed_out": false,
                "settled": outcome.0.len(),
                "activity": outcome.1.len(),
            }));
            return Ok(tool_result);
        }

        if started.elapsed() >= timeout {
            let payload = json!({
                "action": "wait",
                "until": "activity",
                "settled": [],
                "activity": [],
                "running": outcome.2,
                "elapsed_ms": started.elapsed().as_millis(),
                "timed_out": true,
                "note": "The wait interval ended without new child activity. Children keep running after ordinary turn completion and report through <codewhale:subagent.done> sentinels.",
            });
            let mut tool_result = ToolResult::json(&payload)
                .map_err(|err| ToolError::execution_failed(err.to_string()))?;
            tool_result.metadata = Some(json!({ "action": "wait", "timed_out": true }));
            return Ok(tool_result);
        }

        tokio::select! {
            biased;
            () = &mut cancelled => {
                return Err(ToolError::cancelled(
                    "Wait interrupted by user cancellation before child activity.".to_string(),
                ));
            }
            () = tokio::time::sleep(COORD_WAIT_CHECK_INTERVAL) => {}
        }
    }
}

/// Register the narrow coordination tools alongside `agent`.
pub fn register_coordination_tools(
    builder: ToolRegistryBuilder,
    manager: SharedSubAgentManager,
    runtime: SubAgentRuntime,
) -> ToolRegistryBuilder {
    // `runtime.parent_agent_id` is the identity of the agent this registry is
    // being built FOR: `runtime_for_nested_agent_tools` stamps the child's own
    // id there before `new_with_owner` registers tools, so anything that agent
    // spawns records it as parent. Thread that identity through every mutating
    // hierarchy tool: a child may control only its own descendants, while the
    // root registry (`None`) may control any child (TUI-DOG-017).
    let caller = runtime.parent_agent_id.clone();
    let message = AgentsMessageTool::new(Arc::clone(&manager)).with_optional_caller(caller.clone());
    let followup = AgentsFollowupTool::new(Arc::clone(&manager))
        .with_optional_caller(caller.clone())
        .with_runtime(runtime.clone());
    let interrupt =
        AgentsInterruptTool::new(Arc::clone(&manager)).with_optional_caller(caller.clone());
    let coordinate = AgentsCoordinateTool::new(Arc::clone(&manager), caller);
    builder
        .with_tool(Arc::new(AgentsListTool::new(Arc::clone(&manager))))
        .with_tool(Arc::new(message))
        .with_tool(Arc::new(followup))
        .with_tool(Arc::new(interrupt))
        .with_tool(Arc::new(coordinate))
        .with_tool(Arc::new(AgentsWaitTool::new(manager)))
}

pub struct AgentsCoordinateTool {
    manager: SharedSubAgentManager,
    caller: Option<String>,
}

impl AgentsCoordinateTool {
    #[must_use]
    pub fn new(manager: SharedSubAgentManager, caller: Option<String>) -> Self {
        Self { manager, caller }
    }
    fn mutate_coordination(
        caller: Option<&str>,
        session: &str,
        workspace: &std::path::Path,
        manager: &mut super::SubAgentManager,
        input: Value,
    ) -> Result<ToolResult, ToolError> {
        let action = input
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or("inspect");
        let owner = caller.map(str::to_string).unwrap_or_else(|| "root".into());
        let bounded_text = |key: &str| {
            input
                .get(key)
                .and_then(Value::as_str)
                .map(|value| value.chars().take(512).collect::<String>())
        };
        let strings = |key: &str| {
            input
                .get(key)
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .take(24)
                        .filter_map(Value::as_str)
                        .map(|value| value.chars().take(512).collect::<String>())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        };
        if let Some(caller) = caller {
            manager
                .get_result_by_ref_for_session(session, caller)
                .map_err(|_| {
                    ToolError::invalid_input("Agent not found in the active session".to_string())
                })?;
        }
        if matches!(action, "accept" | "supersede") {
            let decision_id = bounded_text("decision_id").unwrap_or_default();
            if !manager.coordination_decision_is_owned_by_session(session, &decision_id) {
                return Err(ToolError::invalid_input(
                    "Coordination decision not found in the active session".to_string(),
                ));
            }
        }
        if action == "reconcile"
            && strings("input_decisions").iter().any(|decision_id| {
                !manager.coordination_decision_is_owned_by_session(session, decision_id)
            })
        {
            return Err(ToolError::invalid_input(
                "One or more coordination decisions were not found in the active session"
                    .to_string(),
            ));
        }
        let coordination_before = manager.coordination.clone();
        let mutation = match action {
            "propose" => manager
                .record_coordination_decision_in_workspace(
                    DecisionRecord {
                        decision_id: bounded_text("decision_id").unwrap_or_default(),
                        subject: bounded_text("subject").unwrap_or_default(),
                        status: DecisionStatus::Proposed,
                        owner,
                        scope: strings("scope"),
                        constraints: strings("constraints"),
                        evidence_handles: strings("evidence_handles"),
                        version: 1,
                        sequence: 0,
                    },
                    workspace,
                )
                .map_err(ToolError::invalid_input)
                .and_then(|record| {
                    serde_json::to_value(record)
                        .map_err(|e| ToolError::execution_failed(e.to_string()))
                }),
            "accept" | "supersede" => input
                .get("expected_version")
                .and_then(Value::as_u64)
                .and_then(|value| u32::try_from(value).ok())
                .ok_or_else(|| {
                    ToolError::invalid_input(
                        "accept/supersede requires expected_version".to_string(),
                    )
                })
                .and_then(|expected_version| {
                    manager
                        .update_coordination_decision(
                            &bounded_text("decision_id").unwrap_or_default(),
                            if action == "accept" {
                                DecisionStatus::Accepted
                            } else {
                                DecisionStatus::Superseded
                            },
                            &owner,
                            expected_version,
                        )
                        .map_err(ToolError::invalid_input)
                })
                .and_then(|record| {
                    serde_json::to_value(record)
                        .map_err(|e| ToolError::execution_failed(e.to_string()))
                }),
            "claim" => manager
                .expand_write_claim(
                    &owner,
                    strings("roots"),
                    strings("exact_files"),
                    strings("contracts"),
                )
                .map_err(ToolError::invalid_input)
                .and_then(|claim| {
                    serde_json::to_value(claim)
                        .map_err(|e| ToolError::execution_failed(e.to_string()))
                }),
            "reconcile" => manager
                .reconcile_coordination(
                    bounded_text("subject").unwrap_or_default(),
                    owner,
                    strings("input_decisions"),
                    bounded_text("outcome").unwrap_or_default(),
                    strings("evidence_handles"),
                    strings("candidate_handles"),
                    input
                        .get("retry_count")
                        .and_then(Value::as_u64)
                        .and_then(|value| u32::try_from(value).ok())
                        .unwrap_or_default(),
                    input
                        .get("retry_limit")
                        .and_then(Value::as_u64)
                        .and_then(|value| u32::try_from(value).ok())
                        .unwrap_or(MAX_RECONCILIATION_RETRIES),
                    strings("reviewer_evidence_handles"),
                    strings("verifier_evidence_handles"),
                    bounded_text("verification_outcome").unwrap_or_default(),
                )
                .map_err(ToolError::invalid_input)
                .and_then(|receipt| {
                    serde_json::to_value(receipt)
                        .map_err(|e| ToolError::execution_failed(e.to_string()))
                }),
            "release" => {
                let owner = input
                    .get("owner")
                    .and_then(Value::as_str)
                    .map(|value| value.to_string());
                let released = manager
                    .release_stale_write_claims(owner)
                    .map_err(ToolError::invalid_input)?;
                let sequence = manager.coordination.sequence;
                serde_json::to_value(json!({
                    "released": released.len(),
                    "owners": released,
                    "sequence": sequence
                }))
                .map_err(|e| ToolError::execution_failed(e.to_string()))
            }
            _ => unreachable!("coordination action validated above"),
        };
        let value = match mutation {
            Ok(value) => value,
            Err(error) => {
                // Contention failures deliberately append a durable receipt.
                // Stamp and persist every sequence allocated by the failed
                // action before returning its error; validation failures that
                // did not mutate the ledger allocate nothing.
                let first_new_sequence = coordination_before.sequence.saturating_add(1);
                let last_new_sequence = manager.coordination.sequence;
                for sequence in first_new_sequence..=last_new_sequence {
                    if let Err(stamp_error) =
                        manager.stamp_coordination_sequence_for_session(sequence, session)
                    {
                        manager.coordination = coordination_before;
                        return Err(ToolError::execution_failed(format!(
                            "{error}; additionally failed to stamp coordination receipt: {stamp_error}"
                        )));
                    }
                }
                if last_new_sequence >= first_new_sequence
                    && let Err(persist_error) = manager.persist_state_synchronously()
                {
                    manager.coordination = coordination_before;
                    return Err(ToolError::execution_failed(format!(
                        "{error}; additionally failed to persist coordination receipt: {persist_error}"
                    )));
                }
                return Err(error);
            }
        };
        let Some(sequence) = value.get("sequence").and_then(Value::as_u64) else {
            manager.coordination = coordination_before;
            return Err(ToolError::execution_failed(format!(
                "coordination action '{action}' produced no durable sequence"
            )));
        };
        if let Err(error) = manager.stamp_coordination_sequence_for_session(sequence, session) {
            manager.coordination = coordination_before;
            return Err(ToolError::execution_failed(error));
        }
        if let Err(error) = manager.persist_state_synchronously() {
            manager.coordination = coordination_before;
            return Err(ToolError::execution_failed(format!(
                "failed to persist coordination action '{action}': {error}"
            )));
        }
        ToolResult::json(&value).map_err(|e| ToolError::execution_failed(e.to_string()))
    }
}

#[async_trait]
impl ToolSpec for AgentsCoordinateTool {
    fn model_visible(&self) -> bool {
        // #5462: `agent` is the sole model-facing sub-agent surface. These
        // narrow tools stay registered and executable by name so a persisted
        // transcript replays byte-for-byte, but they are never advertised in
        // the catalog and can never be returned by `tool_search` — the same
        // shape `rlm` and `exec_shell` already use.
        false
    }

    fn name(&self) -> &'static str {
        "agents/coordinate"
    }

    fn description(&self) -> &'static str {
        "Record or inspect bounded coordination state: propose/accept/supersede decisions, expand the caller's write claim before mutation, reconcile multiple decision records into one neutral fan-in receipt, or release stale write-claims whose owner is no longer running."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": { "type": "string", "enum": ["inspect", "propose", "accept", "supersede", "claim", "reconcile", "release"] },
                "decision_id": { "type": "string" },
                "subject": { "type": "string" },
                "expected_version": { "type": "integer", "minimum": 1 },
                "scope": { "type": "array", "items": { "type": "string" } },
                "constraints": { "type": "array", "items": { "type": "string" } },
                "evidence_handles": { "type": "array", "items": { "type": "string" } },
                "roots": { "type": "array", "items": { "type": "string" } },
                "exact_files": { "type": "array", "items": { "type": "string" } },
                "contracts": { "type": "array", "items": { "type": "string" } },
                "owner": { "type": "string" },
                "input_decisions": { "type": "array", "items": { "type": "string" } },
                "outcome": { "type": "string" },
                "candidate_handles": { "type": "array", "items": { "type": "string" } },
                "retry_count": { "type": "integer", "minimum": 0, "maximum": 3 },
                "retry_limit": { "type": "integer", "minimum": 1, "maximum": 3 },
                "reviewer_evidence_handles": { "type": "array", "items": { "type": "string" } },
                "verifier_evidence_handles": { "type": "array", "items": { "type": "string" } },
                "verification_outcome": { "type": "string" },
                "limit": { "type": "integer", "minimum": 1, "maximum": 24 }
            },
            "required": ["action"]
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        // #5123-class: this tool mutates the coordination ledger and expands
        // the caller's write claim (actions propose/accept/supersede/claim/
        // reconcile) — declaring ReadOnly was a lie that let policy layers
        // treat a mutating call as a safe read. Only `inspect` is read-only,
        // which is what is_read_only_for reports.
        vec![ToolCapability::WritesFiles]
    }
    fn approval_requirement(&self) -> ApprovalRequirement {
        // Stays Auto: coordination records are session-scoped in-memory
        // state, and gating them would deadlock autonomous sub-agent fan-in.
        ApprovalRequirement::Auto
    }
    fn is_read_only_for(&self, input: &Value) -> bool {
        input.get("action").and_then(Value::as_str) == Some("inspect")
    }

    async fn execute(&self, input: Value, context: &ToolContext) -> Result<ToolResult, ToolError> {
        let action = input
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or("inspect");
        let bounded_text = |key: &str| {
            input
                .get(key)
                .and_then(Value::as_str)
                .map(|value| value.chars().take(512).collect::<String>())
        };
        if action == "inspect" {
            let manager = self.manager.read().await;
            let value = manager.inspect_coordination_for_session(
                &context.state_namespace,
                bounded_text("subject").as_deref(),
                input
                    .get("limit")
                    .and_then(Value::as_u64)
                    .unwrap_or(COORDINATION_INSPECT_LIMIT as u64) as usize,
            );
            return ToolResult::json(&value)
                .map_err(|e| ToolError::execution_failed(e.to_string()));
        }
        if !matches!(
            action,
            "propose" | "accept" | "supersede" | "claim" | "reconcile" | "release"
        ) {
            return Err(ToolError::invalid_input(format!(
                "unknown coordination action '{action}'"
            )));
        }

        let mut manager = self.manager.clone().write_owned().await;
        if matches!(action, "claim" | "propose") {
            let caller = self.caller.clone();
            let session = context.state_namespace.clone();
            let workspace = context.workspace.clone();
            return codewhale_runtime::runtime_control::owner_work(move || {
                Ok(Self::mutate_coordination(
                    caller.as_deref(),
                    &session,
                    &workspace,
                    &mut manager,
                    input,
                ))
            })
            .await
            .map_err(|error| ToolError::execution_failed(error.to_string()))?;
        }
        Self::mutate_coordination(
            self.caller.as_deref(),
            &context.state_namespace,
            &context.workspace,
            &mut manager,
            input,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::spec::ToolContext;
    use codewhale_models::Role;
    use std::collections::BTreeSet;
    use tempfile::tempdir;

    #[test]
    fn coordinate_tool_does_not_declare_read_only() {
        // #5123-class: the tool mutates the coordination ledger and expands
        // write claims; its declared capabilities must not say ReadOnly.
        let manager = Arc::new(tokio::sync::RwLock::new(
            super::super::SubAgentManager::new(std::path::PathBuf::from("."), 1),
        ));
        let tool = AgentsCoordinateTool::new(manager, None);
        let capabilities = ToolSpec::capabilities(&tool);
        assert!(
            !capabilities.contains(&ToolCapability::ReadOnly),
            "agents/coordinate mutates the ledger — ReadOnly is a lie: {capabilities:?}"
        );
        // …but the dynamic check still marks inspect as read-only.
        assert!(tool.is_read_only_for(&json!({"action": "inspect"})));
        assert!(!tool.is_read_only_for(&json!({"action": "propose"})));
    }

    #[test]
    fn coordination_descriptions_match_implemented_resume_behavior() {
        // Checkpoint resume is implemented (#5242): the descriptions must
        // describe the real behavior, including the honest queue-only
        // fallback when no runtime is attached.
        let manager = Arc::new(tokio::sync::RwLock::new(
            super::super::SubAgentManager::new(std::env::temp_dir(), 1),
        ));
        let message = AgentsMessageTool::new(Arc::clone(&manager));
        let followup = AgentsFollowupTool::new(manager);

        assert!(!message.description().contains("natural resume"));
        assert!(message.description().contains("stays queued"));
        assert!(followup.description().contains("attempt to resume"));
        assert!(
            followup
                .description()
                .contains("resumed from their checkpoint")
        );
        assert!(followup.description().contains("queue-only semantics"));
    }

    async fn manager_with_running_child(
        workspace: &std::path::Path,
    ) -> (SharedSubAgentManager, String) {
        let manager = Arc::new(tokio::sync::RwLock::new(
            super::super::SubAgentManager::new(workspace.to_path_buf(), 4),
        ));
        let agent_id = {
            let mut guard = manager.write().await;
            guard.insert_test_running_agent("coord_child", workspace)
        };
        (manager, agent_id)
    }

    async fn manager_with_agent_hierarchy(
        workspace: &std::path::Path,
    ) -> (SharedSubAgentManager, String, String, String) {
        let manager = Arc::new(tokio::sync::RwLock::new(
            super::super::SubAgentManager::new(workspace.to_path_buf(), 8),
        ));
        let (parent, child, sibling) = {
            let mut guard = manager.write().await;
            let parent = guard.insert_test_running_agent("hierarchy_parent", workspace);
            let child = guard.insert_test_running_agent("hierarchy_child", workspace);
            let sibling = guard.insert_test_running_agent("hierarchy_sibling", workspace);
            for (agent_id, parent_id) in [
                (&parent, "root"),
                (&child, parent.as_str()),
                (&sibling, "root"),
            ] {
                let record = guard
                    .worker_records
                    .get_mut(agent_id)
                    .expect("hierarchy worker record");
                record.parent_run_id = Some(parent_id.to_string());
                record.spec.parent_run_id = Some(parent_id.to_string());
            }
            (parent, child, sibling)
        };
        (manager, parent, child, sibling)
    }

    #[tokio::test]
    async fn message_queues_without_waking() {
        let tmp = tempdir().unwrap();
        let (manager, agent_id) = manager_with_running_child(tmp.path()).await;
        let tool = AgentsMessageTool::new(Arc::clone(&manager));
        let result = tool
            .execute(
                json!({ "agent_id": agent_id, "message": "hold this" }),
                &ToolContext::new(tmp.path()),
            )
            .await
            .expect("message ok");
        let body: Value = serde_json::from_str(&result.content).unwrap();
        assert_eq!(body["woke"], json!(false));
        assert_eq!(body["queued"], json!(true));
        assert_eq!(body["queue_depth"], json!(1));

        let guard = manager.read().await;
        let depth = guard.queued_mail_depth(&agent_id).unwrap();
        assert_eq!(depth, 1);
        assert!(!guard.child_was_woken(&agent_id));
    }

    #[tokio::test]
    async fn followup_does_not_claim_wake_when_live_channel_is_closed() {
        let tmp = tempdir().unwrap();
        let (manager, agent_id) = manager_with_running_child(tmp.path()).await;
        let result = AgentsFollowupTool::new(Arc::clone(&manager))
            .execute(
                json!({ "agent_id": agent_id, "message": "try to wake" }),
                &ToolContext::new(tmp.path()),
            )
            .await
            .expect("truthful closed-channel receipt");
        let body: Value = serde_json::from_str(&result.content).unwrap();
        assert_eq!(body["woke"], json!(false));
        assert_eq!(body["queue_depth"], json!(1));
        assert!(
            body["note"].as_str().unwrap_or_default().contains("closed"),
            "{body}"
        );

        let guard = manager.read().await;
        assert_eq!(guard.queued_mail_depth(&agent_id), Some(1));
        assert!(!guard.child_was_woken(&agent_id));
    }

    #[tokio::test]
    async fn hierarchy_mutations_allow_own_descendants_and_deny_siblings_or_ancestors() {
        let tmp = tempdir().unwrap();
        let (manager, parent, child, sibling) = manager_with_agent_hierarchy(tmp.path()).await;
        let context = ToolContext::new(tmp.path());

        AgentsMessageTool::new(Arc::clone(&manager))
            .with_optional_caller(Some(parent.clone()))
            .execute(
                json!({ "agent_id": child, "message": "bounded parent note" }),
                &context,
            )
            .await
            .expect("parent may message its own child");
        AgentsFollowupTool::new(Arc::clone(&manager))
            .with_optional_caller(Some(parent.clone()))
            .execute(
                json!({ "agent_id": child, "message": "resume own child" }),
                &context,
            )
            .await
            .expect("parent may follow up its own child");

        let sibling_message = AgentsMessageTool::new(Arc::clone(&manager))
            .with_optional_caller(Some(parent.clone()))
            .execute(
                json!({ "agent_id": sibling, "message": "cross branch" }),
                &context,
            )
            .await
            .expect_err("sibling message must fail closed")
            .to_string();
        assert!(
            sibling_message.contains("own descendants"),
            "{sibling_message}"
        );

        let ancestor_followup = AgentsFollowupTool::new(Arc::clone(&manager))
            .with_optional_caller(Some(child.clone()))
            .execute(
                json!({ "agent_id": parent, "message": "wake ancestor" }),
                &context,
            )
            .await
            .expect_err("ancestor followup must fail closed")
            .to_string();
        assert!(
            ancestor_followup.contains("own descendants"),
            "{ancestor_followup}"
        );

        let sibling_interrupt = AgentsInterruptTool::new(Arc::clone(&manager))
            .with_optional_caller(Some(parent.clone()))
            .execute(json!({ "agent_id": sibling }), &context)
            .await
            .expect_err("sibling interrupt must fail closed")
            .to_string();
        assert!(
            sibling_interrupt.contains("own descendants"),
            "{sibling_interrupt}"
        );

        let interrupted = AgentsInterruptTool::new(Arc::clone(&manager))
            .with_optional_caller(Some(parent))
            .execute(json!({ "agent_id": child }), &context)
            .await
            .expect("parent may interrupt its own child");
        let body: Value = serde_json::from_str(&interrupted.content).unwrap();
        assert_eq!(body["status"], json!("interrupted"));
    }

    #[tokio::test]
    async fn coordinate_inspect_is_side_effect_free_and_mutations_are_synchronously_durable() {
        let tmp = tempdir().unwrap();
        let blocked_state_path = tmp.path().join("blocked-state");
        std::fs::create_dir(&blocked_state_path).unwrap();
        let blocked_manager = Arc::new(tokio::sync::RwLock::new(
            super::super::SubAgentManager::new(tmp.path().to_path_buf(), 4)
                .with_state_path(blocked_state_path),
        ));
        let blocked_tool = AgentsCoordinateTool::new(Arc::clone(&blocked_manager), None);

        blocked_tool
            .execute(
                json!({ "action": "inspect" }),
                &ToolContext::new(tmp.path()),
            )
            .await
            .expect("read-only inspect must not attempt persistence");
        let error = blocked_tool
            .execute(
                json!({
                    "action": "propose",
                    "decision_id": "durable-decision",
                    "subject": "durability",
                    "constraints": ["persist before acknowledgement"]
                }),
                &ToolContext::new(tmp.path()),
            )
            .await
            .expect_err("mutation must fail when its receipt cannot persist")
            .to_string();
        assert!(error.contains("failed to persist"), "{error}");
        assert!(
            blocked_manager
                .read()
                .await
                .coordination
                .decisions
                .is_empty(),
            "failed persistence must roll the in-memory decision back"
        );

        let durable_workspace = tempdir().unwrap();
        let state_path = durable_workspace.path().join("subagents.v1.json");
        let manager = Arc::new(tokio::sync::RwLock::new(
            super::super::SubAgentManager::new(durable_workspace.path().to_path_buf(), 4)
                .with_state_path(state_path.clone()),
        ));
        AgentsCoordinateTool::new(Arc::clone(&manager), None)
            .execute(
                json!({
                    "action": "propose",
                    "decision_id": "durable-decision",
                    "subject": "durability",
                    "constraints": ["persist before acknowledgement"]
                }),
                &ToolContext::new(durable_workspace.path()),
            )
            .await
            .expect("durable mutation");
        let mut replayed =
            super::super::SubAgentManager::new(durable_workspace.path().to_path_buf(), 4)
                .with_state_path(state_path);
        replayed.load_state().expect("reload durable action");
        assert_eq!(replayed.coordination.decisions.len(), 1);
        assert_eq!(
            replayed.coordination.decisions[0].decision_id,
            "durable-decision"
        );
    }

    #[tokio::test]
    async fn release_action_clears_only_stale_write_claims_and_persists() {
        let tmp = tempdir().unwrap();
        let state_path = tmp.path().join("subagents.v1.json");
        let manager = Arc::new(tokio::sync::RwLock::new(
            super::super::SubAgentManager::new(tmp.path().to_path_buf(), 4)
                .with_state_path(state_path.clone()),
        ));
        let live = {
            let mut guard = manager.write().await;
            let live = guard.insert_test_running_agent("live-builder", tmp.path());
            let active = [live.clone()].into_iter().collect::<BTreeSet<_>>();
            for claim in [
                WriteScopeClaim {
                    owner: live.clone(),
                    roots: vec!["src/live".into()],
                    exact_files: Vec::new(),
                    contracts: Vec::new(),
                },
                WriteScopeClaim {
                    owner: "zombie-builder".into(),
                    roots: vec!["src/zombie".into()],
                    exact_files: Vec::new(),
                    contracts: Vec::new(),
                },
            ] {
                guard
                    .coordination
                    .register_claim(claim, false, |candidate| active.contains(candidate))
                    .expect("initial claim");
            }
            let _ = guard.persist_state_synchronously();
            live
        };

        let result = AgentsCoordinateTool::new(Arc::clone(&manager), None)
            .execute(
                json!({ "action": "release" }),
                &ToolContext::new(tmp.path()),
            )
            .await
            .expect("release sweeps stale claims");
        let body: Value = serde_json::from_str(&result.content).unwrap();
        assert_eq!(body["released"], json!(1));
        assert_eq!(body["owners"], json!(["zombie-builder"]));

        // The sweep is durable: reloading the ledger keeps only the live claim.
        let mut replayed = super::super::SubAgentManager::new(tmp.path().to_path_buf(), 4)
            .with_state_path(state_path);
        replayed.load_state().expect("reload released ledger");
        assert_eq!(replayed.coordination.write_claims.len(), 1);
        assert_eq!(replayed.coordination.write_claims[0].claim.owner, live);

        // Named-owner release of a live claimant is a no-op and still succeeds.
        let noop = AgentsCoordinateTool::new(Arc::clone(&manager), None)
            .execute(
                json!({ "action": "release", "owner": live }),
                &ToolContext::new(tmp.path()),
            )
            .await
            .expect("live release is a no-op");
        let body: Value = serde_json::from_str(&noop.content).unwrap();
        assert_eq!(body["released"], json!(0));
    }

    #[tokio::test]
    async fn rejected_claim_contention_is_persisted_before_returning_the_error() {
        let tmp = tempdir().unwrap();
        let state_path = tmp.path().join("subagents.v1.json");
        let manager = Arc::new(tokio::sync::RwLock::new(
            super::super::SubAgentManager::new(tmp.path().to_path_buf(), 4)
                .with_state_path(state_path.clone()),
        ));
        let (claimant, owner) = {
            let mut guard = manager.write().await;
            let claimant = guard.insert_test_running_agent("claimant", tmp.path());
            let owner = guard.insert_test_running_agent("owner", tmp.path());
            let active = [claimant.clone(), owner.clone()]
                .into_iter()
                .collect::<BTreeSet<_>>();
            for claim in [
                WriteScopeClaim {
                    owner: claimant.clone(),
                    roots: vec!["src/claimant".into()],
                    exact_files: Vec::new(),
                    contracts: Vec::new(),
                },
                WriteScopeClaim {
                    owner: owner.clone(),
                    roots: vec!["src/shared".into()],
                    exact_files: Vec::new(),
                    contracts: Vec::new(),
                },
            ] {
                guard
                    .coordination
                    .register_claim(claim, false, |candidate| active.contains(candidate))
                    .expect("initial non-overlapping claim");
            }
            (claimant, owner)
        };

        let error = AgentsCoordinateTool::new(Arc::clone(&manager), Some(claimant.clone()))
            .execute(
                json!({ "action": "claim", "roots": ["src/shared/nested"] }),
                &ToolContext::new(tmp.path()),
            )
            .await
            .expect_err("overlap must block")
            .to_string();
        assert!(
            error.contains(&owner) && error.contains("contention"),
            "{error}"
        );

        let mut replayed = super::super::SubAgentManager::new(tmp.path().to_path_buf(), 4)
            .with_state_path(state_path);
        replayed.load_state().expect("reload contention receipt");
        assert_eq!(replayed.coordination.contentions.len(), 1);
        assert_eq!(replayed.coordination.contentions[0].claimant, claimant);
        assert_eq!(
            replayed.coordination.contentions[0].conflicting_owner,
            owner
        );
    }

    #[tokio::test]
    async fn coordination_resolution_survives_reload_and_resolving_claim_eviction() {
        let tmp = tempdir().unwrap();
        let state_path = tmp.path().join("subagents.v1.json");
        let manager = Arc::new(tokio::sync::RwLock::new(
            super::super::SubAgentManager::new(tmp.path().to_path_buf(), 4)
                .with_state_path(state_path.clone()),
        ));
        let claimant = {
            let mut guard = manager.write().await;
            let claimant = guard.insert_test_running_agent("claimant", tmp.path());
            let owner = guard.insert_test_running_agent("owner", tmp.path());
            let active = [claimant.clone(), owner.clone()]
                .into_iter()
                .collect::<BTreeSet<_>>();
            for claim in [
                WriteScopeClaim {
                    owner: claimant.clone(),
                    roots: vec!["src/claimant".into()],
                    exact_files: Vec::new(),
                    contracts: Vec::new(),
                },
                WriteScopeClaim {
                    owner: owner.clone(),
                    roots: vec!["src/shared".into()],
                    exact_files: Vec::new(),
                    contracts: Vec::new(),
                },
            ] {
                guard
                    .coordination
                    .register_claim(claim, false, |candidate| active.contains(candidate))
                    .expect("initial non-overlapping claim");
            }
            claimant
        };

        AgentsCoordinateTool::new(Arc::clone(&manager), Some(claimant.clone()))
            .execute(
                json!({ "action": "claim", "roots": ["src/shared/nested"] }),
                &ToolContext::new(tmp.path()),
            )
            .await
            .expect_err("overlap must block and persist its receipt");

        let resolution_sequence = {
            let mut guard = manager.write().await;
            let record = guard
                .coordination
                .register_claim(
                    WriteScopeClaim {
                        owner: claimant.clone(),
                        roots: vec!["src/isolated".into()],
                        exact_files: Vec::new(),
                        contracts: Vec::new(),
                    },
                    true,
                    |_| true,
                )
                .expect("later isolated claim resolves contention");
            guard
                .persist_state_synchronously()
                .expect("persist resolved contention");
            record.sequence
        };

        let mut replayed = super::super::SubAgentManager::new(tmp.path().to_path_buf(), 4)
            .with_state_path(state_path.clone());
        replayed.load_state().expect("reload resolved contention");
        assert_eq!(replayed.coordination.contentions.len(), 1);
        assert_eq!(
            replayed.coordination.contentions[0].disposition,
            WriteContentionDisposition::ResolvedBySuccessfulClaim
        );
        assert_eq!(
            replayed.coordination.contentions[0].resolution_sequence,
            Some(resolution_sequence)
        );

        let slots = COORDINATION_RECORD_LIMIT - replayed.coordination.write_claims.len();
        for index in 0..slots {
            replayed
                .coordination
                .register_claim(
                    WriteScopeClaim {
                        owner: format!("inactive-fill-{index:03}"),
                        roots: vec![format!("pkg/fill-{index:03}")],
                        exact_files: Vec::new(),
                        contracts: Vec::new(),
                    },
                    true,
                    |_| false,
                )
                .expect("fill inactive claim capacity");
        }
        for index in 0..2 {
            replayed
                .coordination
                .register_claim(
                    WriteScopeClaim {
                        owner: format!("inactive-overflow-{index}"),
                        roots: vec![format!("pkg/overflow-{index}")],
                        exact_files: Vec::new(),
                        contracts: Vec::new(),
                    },
                    true,
                    |_| false,
                )
                .expect("evict oldest inactive claim at capacity");
        }
        assert!(
            !replayed
                .coordination
                .write_claims
                .iter()
                .any(|claim| claim.claim.owner == claimant),
            "the resolving claimant claim must be evicted for the durability regression"
        );
        replayed
            .persist_state_synchronously()
            .expect("persist after inactive claim eviction");

        let mut final_replay = super::super::SubAgentManager::new(tmp.path().to_path_buf(), 4)
            .with_state_path(state_path);
        final_replay
            .load_state()
            .expect("reload after resolving claim eviction");
        let projection = final_replay.coordination_detail_projection(None, 24);
        assert!(
            !projection
                .write_claims
                .iter()
                .any(|claim| claim.claim.owner == claimant)
        );
        assert_eq!(projection.contentions.len(), 1);
        assert_eq!(
            projection.contentions[0].disposition,
            WriteContentionDisposition::ResolvedBySuccessfulClaim
        );
        assert_eq!(
            projection.contentions[0].resolution_sequence,
            Some(resolution_sequence)
        );
    }

    #[tokio::test]
    async fn interrupt_fails_closed_on_self() {
        let tmp = tempdir().unwrap();
        let (manager, agent_id) = manager_with_running_child(tmp.path()).await;
        let tool = AgentsInterruptTool::new(Arc::clone(&manager)).with_caller(agent_id.clone());
        let err = tool
            .execute(
                json!({ "agent_id": agent_id }),
                &ToolContext::new(tmp.path()),
            )
            .await
            .expect_err("self interrupt must fail");
        let msg = err.to_string().to_ascii_lowercase();
        assert!(
            msg.contains("self") || msg.contains("own"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn interrupt_fails_closed_on_missing_target() {
        let tmp = tempdir().unwrap();
        let manager = Arc::new(tokio::sync::RwLock::new(
            super::super::SubAgentManager::new(tmp.path().to_path_buf(), 2),
        ));
        let tool = AgentsInterruptTool::new(manager);
        let err = tool
            .execute(
                json!({ "agent_id": "agent_missing" }),
                &ToolContext::new(tmp.path()),
            )
            .await
            .expect_err("missing target");
        assert!(err.to_string().contains("not found") || err.to_string().contains("Agent"));
    }

    #[tokio::test]
    async fn wait_times_out_when_child_stays_running() {
        let tmp = tempdir().unwrap();
        let (manager, agent_id) = manager_with_running_child(tmp.path()).await;
        let tool = AgentsWaitTool::new(manager);
        let result = tool
            .execute(
                json!({
                    "agent_id": agent_id,
                    "timeout_secs": 1,
                    "until": "activity"
                }),
                &ToolContext::new(tmp.path()),
            )
            .await
            .expect("wait returns");
        let body: Value = serde_json::from_str(&result.content).unwrap();
        assert_eq!(body["timed_out"], json!(true));
    }

    #[tokio::test]
    async fn list_resolves_target_and_reports_queue() {
        let tmp = tempdir().unwrap();
        let (manager, agent_id) = manager_with_running_child(tmp.path()).await;
        {
            let mut guard = manager.write().await;
            guard
                .queue_parent_message(&agent_id, "note".into(), false)
                .unwrap();
        }
        let tool = AgentsListTool::new(manager);
        let result = tool
            .execute(
                json!({ "agent_id": agent_id }),
                &ToolContext::new(tmp.path()),
            )
            .await
            .expect("list ok");
        let body: Value = serde_json::from_str(&result.content).unwrap();
        assert_eq!(body["count"], json!(1));
        assert_eq!(body["agents"][0]["agent_id"], json!(agent_id));
        assert!(body["agents"][0]["queued_mail"].as_u64().unwrap_or(0) >= 1);
    }

    #[tokio::test]
    async fn followup_interrupted_continuable_without_runtime_queues_honestly() {
        let tmp = tempdir().unwrap();
        let manager = Arc::new(tokio::sync::RwLock::new(
            super::super::SubAgentManager::new(tmp.path().to_path_buf(), 4),
        ));
        let (agent_id, handle) = {
            let mut guard = manager.write().await;
            guard.insert_test_interrupted_continuable_agent(
                "paused_child",
                tmp.path(),
                vec![codewhale_models::Message {
                    role: Role::User,
                    content: vec![codewhale_models::ContentBlock::Text {
                        text: "prior work".to_string(),
                        cache_control: None,
                    }],
                }],
            )
        };
        // No runtime attached: checkpoint resume is unavailable, so followup
        // keeps the honest queue-only semantics with the continuation handle.
        let tool = AgentsFollowupTool::new(Arc::clone(&manager));
        let result = tool
            .execute(
                json!({ "agent_id": agent_id, "message": "please continue" }),
                &ToolContext::new(tmp.path()),
            )
            .await
            .expect("followup ok");
        let body: Value = serde_json::from_str(&result.content).unwrap();
        assert_eq!(body["queued"], json!(true));
        assert_eq!(body["woke"], json!(false));
        assert_eq!(body["continued_from_checkpoint"], json!(false));
        assert_eq!(body["continuation_handle"], json!(handle));
        let note = body["note"].as_str().unwrap_or_default();
        assert!(
            note.contains("attach a runtime") && note.contains(&handle),
            "note must point at the resume path with the continuation handle: {note}"
        );

        let guard = manager.read().await;
        assert_eq!(guard.queued_mail_depth(&agent_id).unwrap(), 1);
        assert!(!guard.child_was_woken(&agent_id));
    }

    // === until="all": the fan-out join ===================================
    //
    // Before this existed a parent with five children had to issue five
    // waits — while the prompt told it not to poll. These lock the join in.

    fn empty_manager(workspace: &std::path::Path) -> SharedSubAgentManager {
        Arc::new(tokio::sync::RwLock::new(
            super::super::SubAgentManager::new(workspace.to_path_buf(), 8),
        ))
    }

    async fn settle(manager: &SharedSubAgentManager, agent_id: &str, status: SubAgentStatus) {
        let mut guard = manager.write().await;
        if let Some(agent) = guard.agents.get_mut(agent_id) {
            agent.status = status;
        }
    }

    #[test]
    fn wait_schema_offers_all_as_a_first_class_until() {
        let tmp = tempdir().unwrap();
        let tool = AgentsWaitTool::new(empty_manager(tmp.path()));
        let schema = tool.input_schema();
        let until = &schema["properties"]["until"];
        assert_eq!(
            until["enum"],
            json!(["completion", "all", "activity"]),
            "until must expose all alongside completion/activity: {schema}"
        );
        let described = until["description"].as_str().unwrap_or_default();
        assert!(
            described.contains("every watched child") && described.contains("any one child"),
            "the schema must make completion vs all unmistakable: {described}"
        );
    }

    #[tokio::test]
    async fn wait_until_all_on_an_already_settled_child_reports_its_outcome() {
        let tmp = tempdir().unwrap();
        let manager = empty_manager(tmp.path());
        let agent_id = {
            let mut guard = manager.write().await;
            guard.insert_test_running_agent("all_already_done", tmp.path())
        };
        settle(&manager, &agent_id, SubAgentStatus::Completed).await;

        let result = dispatch_wait(
            &json!({ "until": "all", "agent_id": agent_id, "timeout_secs": 60 }),
            Arc::clone(&manager),
            &ToolContext::new(tmp.path()),
        )
        .await
        .expect("a settled child is an immediate return");
        let body: Value = serde_json::from_str(&result.content).unwrap();
        assert_eq!(body["all_settled"], json!(true), "{body}");
        let settled = body["settled"].as_array().unwrap();
        assert_eq!(settled.len(), 1, "{body}");
        assert_eq!(settled[0]["status"], json!("completed"), "{body}");
    }

    #[tokio::test]
    async fn foreign_session_is_excluded_from_default_and_explicit_list_waits() {
        let tmp = tempdir().unwrap();
        let manager = empty_manager(tmp.path());
        let agent_a = {
            let mut guard = manager.write().await;
            let agent_id = guard.insert_test_running_agent("foreign_wait_a", tmp.path());
            guard.assign_test_session_owner(&agent_id, "session-a");
            agent_id
        };
        let context_b = ToolContext::new(tmp.path()).with_state_namespace("session-b");

        let listed = AgentsListTool::new(Arc::clone(&manager))
            .execute(json!({}), &context_b)
            .await
            .expect("B list");
        let listed: Value = serde_json::from_str(&listed.content).unwrap();
        assert_eq!(listed["count"], json!(0));

        let default_wait = dispatch_wait(
            &json!({ "until": "all", "timeout_secs": 60 }),
            Arc::clone(&manager),
            &context_b,
        )
        .await
        .expect("B default wait has no visible children");
        let default_wait: Value = serde_json::from_str(&default_wait.content).unwrap();
        assert!(default_wait["settled"].as_array().unwrap().is_empty());

        let error = dispatch_wait(
            &json!({ "until": "all", "agent_id": agent_a, "timeout_secs": 60 }),
            manager,
            &context_b,
        )
        .await
        .expect_err("B explicit wait must reject A")
        .to_string();
        assert!(
            error.contains("Agent not found in the active session"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn wait_until_all_returns_immediately_with_no_children() {
        let tmp = tempdir().unwrap();
        let started = Instant::now();
        let result = dispatch_wait(
            &json!({ "until": "all", "timeout_secs": 60 }),
            empty_manager(tmp.path()),
            &ToolContext::new(tmp.path()),
        )
        .await
        .expect("wait-for-all with zero children must return, not hang");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "zero children must not burn the timeout"
        );
        let body: Value = serde_json::from_str(&result.content).unwrap();
        assert_eq!(body["all_settled"], json!(true));
        assert_eq!(body["timed_out"], json!(false));
        assert!(body["settled"].as_array().unwrap().is_empty(), "{body}");
    }

    #[tokio::test]
    async fn wait_until_all_blocks_until_every_child_settles() {
        let tmp = tempdir().unwrap();
        let manager = empty_manager(tmp.path());
        let (first, second, third) = {
            let mut guard = manager.write().await;
            (
                guard.insert_test_running_agent("all_first", tmp.path()),
                guard.insert_test_running_agent("all_second", tmp.path()),
                guard.insert_test_running_agent("all_third", tmp.path()),
            )
        };

        // Staggered settles: an `until=completion` wait would return after the
        // first one. `until=all` must stay blocked through the last.
        let flip = Arc::clone(&manager);
        let (a, b, c) = (first.clone(), second.clone(), third.clone());
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            settle(&flip, &a, SubAgentStatus::Completed).await;
            tokio::time::sleep(Duration::from_millis(150)).await;
            settle(&flip, &b, SubAgentStatus::Failed("boom".to_string())).await;
            tokio::time::sleep(Duration::from_millis(150)).await;
            settle(&flip, &c, SubAgentStatus::Cancelled).await;
        });

        let result = dispatch_wait(
            &json!({ "until": "all", "timeout_secs": 30 }),
            Arc::clone(&manager),
            &ToolContext::new(tmp.path()),
        )
        .await
        .expect("wait-for-all should succeed");
        let body: Value = serde_json::from_str(&result.content).unwrap();
        assert_eq!(body["all_settled"], json!(true), "{body}");
        assert_eq!(body["timed_out"], json!(false), "{body}");
        assert!(
            body["still_running"].as_array().unwrap().is_empty(),
            "{body}"
        );

        // Per-child outcomes come back on the single return.
        let settled = body["settled"].as_array().unwrap();
        assert_eq!(settled.len(), 3, "{body}");
        let outcomes: std::collections::BTreeMap<&str, &str> = settled
            .iter()
            .map(|entry| {
                (
                    entry["agent_id"].as_str().unwrap(),
                    entry["status"].as_str().unwrap(),
                )
            })
            .collect();
        assert_eq!(outcomes.get(first.as_str()), Some(&"completed"), "{body}");
        assert_eq!(outcomes.get(second.as_str()), Some(&"failed"), "{body}");
        assert_eq!(outcomes.get(third.as_str()), Some(&"cancelled"), "{body}");
    }

    #[tokio::test]
    async fn wait_until_all_times_out_reporting_settled_and_still_running() {
        let tmp = tempdir().unwrap();
        let manager = empty_manager(tmp.path());
        let (done, stuck) = {
            let mut guard = manager.write().await;
            (
                guard.insert_test_running_agent("all_done", tmp.path()),
                guard.insert_test_running_agent("all_stuck", tmp.path()),
            )
        };

        let request = json!({ "until": "all", "timeout_secs": 1 });
        let context = ToolContext::new(tmp.path());
        let wait = dispatch_wait(&request, Arc::clone(&manager), &context);
        tokio::pin!(wait);
        // Capture both running children before completing one. The timeout
        // receipt must not depend on a background task winning a 50ms race.
        assert!(futures_util::poll!(wait.as_mut()).is_pending());
        settle(&manager, &done, SubAgentStatus::Completed).await;

        let result = wait
            .await
            .expect("a timeout is a partial receipt, not an error");
        let body: Value = serde_json::from_str(&result.content).unwrap();
        assert_eq!(body["timed_out"], json!(true), "{body}");
        assert_eq!(body["all_settled"], json!(false), "{body}");

        let settled = body["settled"].as_array().unwrap();
        assert_eq!(settled.len(), 1, "{body}");
        assert_eq!(settled[0]["agent_id"], json!(done), "{body}");
        assert_eq!(settled[0]["status"], json!("completed"), "{body}");

        let running = body["still_running"].as_array().unwrap();
        assert_eq!(running.len(), 1, "{body}");
        assert_eq!(running[0]["agent_id"], json!(stuck), "{body}");
    }

    #[tokio::test]
    async fn wait_until_all_ignores_children_spawned_mid_wait() {
        let tmp = tempdir().unwrap();
        let manager = empty_manager(tmp.path());
        let original = {
            let mut guard = manager.write().await;
            guard.insert_test_running_agent("all_original", tmp.path())
        };

        let flip = Arc::clone(&manager);
        let tmp_path = tmp.path().to_path_buf();
        let original_id = original.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            {
                let mut guard = flip.write().await;
                guard.insert_test_running_agent("all_latecomer", &tmp_path);
            }
            settle(&flip, &original_id, SubAgentStatus::Completed).await;
        });

        let result = dispatch_wait(
            &json!({ "until": "all", "timeout_secs": 30 }),
            Arc::clone(&manager),
            &ToolContext::new(tmp.path()),
        )
        .await
        .expect("wait-for-all should succeed");
        let body: Value = serde_json::from_str(&result.content).unwrap();
        // The watch set is the batch as of call time: the latecomer must not
        // extend a wait the caller never asked to include it in.
        assert_eq!(body["all_settled"], json!(true), "{body}");
        assert_eq!(body["timed_out"], json!(false), "{body}");
        let settled = body["settled"].as_array().unwrap();
        assert_eq!(settled.len(), 1, "{body}");
        assert_eq!(settled[0]["agent_id"], json!(original), "{body}");
    }

    #[tokio::test]
    async fn wait_rejects_unknown_until_naming_every_supported_mode() {
        let tmp = tempdir().unwrap();
        let error = dispatch_wait(
            &json!({ "until": "forever" }),
            empty_manager(tmp.path()),
            &ToolContext::new(tmp.path()),
        )
        .await
        .expect_err("an unknown until must fail loudly");
        let message = error.to_string();
        for mode in ["completion", "all", "activity"] {
            assert!(message.contains(mode), "{message}");
        }
    }

    #[tokio::test]
    async fn followup_interrupted_continuable_resumes_with_runtime() {
        let tmp = tempdir().unwrap();
        let manager = Arc::new(tokio::sync::RwLock::new(
            super::super::SubAgentManager::new(tmp.path().to_path_buf(), 4),
        ));
        let (agent_id, _handle) = {
            let mut guard = manager.write().await;
            let interrupted = guard.insert_test_interrupted_continuable_agent(
                "paused_child",
                tmp.path(),
                vec![codewhale_models::Message {
                    role: Role::User,
                    content: vec![codewhale_models::ContentBlock::Text {
                        text: "prior work".to_string(),
                        cache_control: None,
                    }],
                }],
            );
            // This fixture is resumed by root, rather than a fabricated parent_session agent.
            let record = guard.worker_records.get_mut(&interrupted.0).unwrap();
            record.parent_run_id = None;
            record.spec.parent_run_id = None;
            interrupted
        };
        let mut runtime = super::super::tests::stub_runtime();
        runtime.manager = Arc::clone(&manager);
        runtime.context = ToolContext::new(tmp.path());
        let tool = AgentsFollowupTool::new(Arc::clone(&manager)).with_runtime(runtime);
        let result = tool
            .execute(
                json!({ "agent_id": agent_id, "message": "please continue" }),
                &ToolContext::new(tmp.path()),
            )
            .await
            .expect("followup ok");
        let body: Value = serde_json::from_str(&result.content).unwrap();
        assert_eq!(body["queued"], json!(true));
        assert_eq!(body["woke"], json!(true));
        assert_eq!(body["continued_from_checkpoint"], json!(true));
        let note = body["note"].as_str().unwrap_or_default();
        assert!(note.contains("resumed from checkpoint"), "{note}");
        let resumed_id = body["agent_id"].as_str().unwrap_or_default();
        assert_ne!(
            resumed_id, agent_id,
            "resume re-dispatches under a new agent id"
        );

        // A fresh record exists for the resumed session; the prior terminal
        // record stays immutable (receipts are never rewritten).
        let guard = manager.read().await;
        guard.get_result(resumed_id).expect("resumed agent exists");
        let prior = guard.get_result(&agent_id).expect("prior record");
        assert!(matches!(prior.status, SubAgentStatus::Interrupted(_)));
    }
}

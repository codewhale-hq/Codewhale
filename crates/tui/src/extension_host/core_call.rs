//! `core/call`: an extension tool, while it handles a `tool/call`, asking the
//! core to run one of the core's tools for it.
//!
//! The call goes through the same gate as a call the model makes. The turn
//! loop serves it (`Engine::gate_nested_call`, source `Extension`): planning
//! (allow and deny lists, preparation, hooks, ask-rules, Auto-Review, repo
//! law, the authority envelope), then the approval card when one is needed,
//! then execution by the same machinery code mode uses
//! ([`CodemodeInvoker`]). Nothing the host sends decides any of it: not an
//! approval, not the card's text (Rust composes it, naming the extension and
//! the tool it is inside), not an argv or a URL, not the ticket's contents.
//!
//! **When it exists.** An invocation ticket is minted only for an extension
//! tool call that runs under a nested-call gate the turn loop is serving *for
//! that tool* (`NestedCallGate::extension`, set by the turn loop when the model
//! called the tool directly, in a sub-agent's turn as in the session's). A tool
//! run from inside `execute_tools` (code mode hands nested specs no gate), a
//! command, a timer, activation code and every test without a turn have none,
//! so their `core/call`s are refused ("out of turn"). The ticket lives exactly
//! as long as the invocation: it is revoked when the `tool/call` ends
//! (answered, failed, timed out or dropped), when its owner is revoked and when
//! its host exits, and each of those withdraws the invocation's pending core
//! calls and any approval card one is waiting on.
//!
//! **What is refused outright** ([`refusal`], checked before planning on the
//! name the host sent and again by the turn loop on the name planning resolved
//! it to and the final, hook-rewritten input): everything code mode refuses
//! before its gate (`execute_tools`, interpreters, `agent`, `workflow`, `rlm`,
//! `request_user_input`, interactive shells, sandbox escalation, Computer Use
//! consent and scripts, MCP sign-in); any extension tool (no recursion); tool
//! search and tool-result retrieval; the memory writer and the tools that
//! change what the session may do or schedule work beyond it; and every MCP
//! tool, Computer Use included (not in v1: CURRENT_DECISIONS 26, the founder's
//! recorded default).
//!
//! **What prompts** ([`origin_approval`]): an extension's call needs approval
//! unless the tool is in the small read-only, workspace-local table
//! [`EXT_AUTO_ELIGIBLE`] and planning found nothing that asks; its approval
//! keys are scoped to the extension plugin build
//! (`approval_cache::extension_origin_approval_keys`), so a grant the user gave
//! the model never covers an extension's call and the reverse. Shell and
//! network calls force a prompt: a session grant is not consulted, and
//! Full Access still opens their card. Auto-Review and Never refuse them
//! (`resolve_approval_request_disposition`); explicit denials always win.
//!
//! **Caps.** Per invocation: 50 `core/call`s in all (the ticket's uses), 4 at
//! once (code mode's own cap), and one approval card at a time (the turn
//! loop serves one request at a time). Per host: the channel's 256 in-flight
//! host requests. A burst of invalid tickets ends the host (`ticket`).
//!
//! **Time.** The `tool/call` deadline is measured on the invocation's pausable
//! clock, which stops while a `core/call` waits on the gate, so a person
//! taking a minute on a card does not time the tool out. An approval is never
//! decided for the person: when the host cancels, the owner is revoked, the
//! host exits or the invocation ends, the wait is withdrawn and recorded
//! cancelled.
//!
//! Known limits:
//! * The refusal list names tools; a tool added later that changes the mode,
//!   the posture or the permissions has to be added to [`REFUSED_NAMES`]
//!   (a test fails when a name in it stops being a tool the core registers).
//! * A withdrawn call retires its card through the typed `ApprovalWithdrawn`
//!   event on terminal and runtime clients. A later answer has no live waiter.
//! * Results come back as text (and structured JSON when the tool's content
//!   was JSON); images and other rich blocks are dropped, as in code mode.
//! * One extension tool call at a time holds the turn's tool lock, so a
//!   tool's `core/call`s run beside it, not through the lock.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use codewhale_workflow_js::ToolCallResponse;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::ManagerShared;
use super::protocol::{
    ContentBlockWire, CoreCallParams, RpcErrorWire, SessionCheckpointParams, ToolResultWire,
    UserQuestionParams, error_code,
};
use super::supervisor::HostRequestContext;
use super::ticket::{Grant, Presented, Ticket, TicketKind};
use super::tier::HostTier;
use crate::core::authority::{ToolCategory, get_tool_category_for_call};
use crate::core::events::SessionCheckpointCaller;
use crate::tools::codemode::{
    CodemodeInvoker, ExtensionCaller, NestedCallGate, NestedDecision, NestedFailure, PauseClock,
};
use crate::tools::spec::{ToolCapability, ToolContext, ToolSpec};
use codewhale_config::OwnerRef;

/// The protocol method an invocation ticket is good for.
pub(crate) const METHOD: &str = "core/call";
const CHECKPOINT_METHOD: &str = "session/checkpoint";
const QUESTION_METHOD: &str = "user-questions/ask";
/// `core/call`s one invocation may make in all.
pub(crate) const MAX_CALLS_PER_INVOCATION: u32 = 50;
/// A backstop only: an invocation ends (and revokes its ticket) long before
/// this, and its own deadline pauses while a person decides.
const TICKET_TTL: Duration = Duration::from_secs(24 * 60 * 60);
/// Receipts attached to an extension tool's result metadata.
pub(crate) const RECEIPTS_ATTACHED: usize = 50;

/// Tools an extension may never reach through `core/call`, by lower-case
/// name, checked on the spelling the host sent and on the canonical action
/// alias planning resolves it to.
const REFUSED_NAMES: &[&str] = &[
    // Discovery and retrieval: schema activation and spilled output are the
    // turn's own.
    "tool_search",
    "tool_search_tool_regex",
    "tool_search_tool_bm25",
    "retrieve_tool_result",
    // The memory writer.
    "remember",
    // What the session may do, or work scheduled beyond it.
    "request_plugin_install",
    "create_goal",
    "update_goal",
    "automation",
    "send_later",
    "start_mcp_server",
    "start_registry_mcp_server",
];

/// Read-only, workspace-local tools an extension's call may run without a
/// prompt, when planning also finds nothing that asks for one. Every other
/// tool an extension asks for needs approval. A test pins that each is a
/// registered, read-only, auto-approved tool.
pub(crate) const EXT_AUTO_ELIGIBLE: &[&str] =
    &["read", "read_file", "list_dir", "file_search", "grep_files"];

/// Tools that run commands or reach the network and are not in the shell and
/// network categories by name: an extension's call of one forces a prompt too.
const FORCED_PROMPT_NAMES: &[&str] = &[
    "web.run",
    "git_fetch",
    "finance",
    "run_tests",
    "run_verifiers",
    "verify",
    "harness",
];

fn canonical(name: &str, input: &Value) -> String {
    // Preserve the family's registered spelling until its action is resolved.
    // Lowercasing `Git` first would hide `Git{action:"fetch"}` from policy.
    let family = codewhale_execpolicy::canonical_action::CANONICAL_ACTION_ALIASES
        .iter()
        .find(|(family, _, _)| family.eq_ignore_ascii_case(name))
        .map_or(name, |(family, _, _)| *family);
    codewhale_execpolicy::canonical_action::canonical_action_alias(family, input)
        .to_ascii_lowercase()
}

fn is_extension_tool(specs: &[Arc<dyn ToolSpec>], name: &str) -> bool {
    specs
        .iter()
        .find(|spec| spec.name().eq_ignore_ascii_case(name))
        .is_some_and(|spec| spec.extension_caller().is_some())
}

/// Whether the core refuses an extension's call of `name` outright, and why:
/// code mode's own refusals plus the extension list (module docs). Pure over
/// the tool snapshot, so the turn loop can run it again on what planning
/// resolved. `ExtraRefusal`'s shape.
pub(crate) fn refusal(specs: &[Arc<dyn ToolSpec>], name: &str, input: &Value) -> Option<String> {
    if let Some(note) = crate::tools::codemode::refusal_before_gate(name, input, true) {
        return Some(note);
    }
    let lower = name.to_ascii_lowercase();
    let resolved = canonical(name, input);
    if is_extension_tool(specs, name) || is_extension_tool(specs, &resolved) {
        return Some(format!(
            "`{name}` is an extension tool; an extension's core/call cannot reach another extension's tool (or its own)"
        ));
    }
    if crate::mcp::McpPool::is_mcp_tool(&lower) || crate::mcp::McpPool::is_mcp_tool(&resolved) {
        return Some(format!(
            "`{name}` is an MCP tool (Computer Use included); extensions cannot call MCP tools through core/call"
        ));
    }
    let family = |candidate: &str| {
        REFUSED_NAMES.contains(&candidate)
            || codewhale_tools::names::is_tool_search_tool(candidate)
            || candidate.starts_with("automation_")
    };
    if family(&lower) || family(&resolved) {
        return Some(format!(
            "`{name}` is not available to an extension's core/call (tool search and retrieval, the memory writer, and tools that change what the session may do or schedule work stay the model's own)"
        ));
    }
    None
}

/// How an extension's call of a tool differs from the model's, after planning
/// has decided what the model's call would need.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OriginApproval {
    /// Planning's decision stands (the tool is in [`EXT_AUTO_ELIGIBLE`] and
    /// nothing asked for approval).
    Unchanged,
    /// Needs approval; an extension-scoped session grant may satisfy it.
    Prompt,
    /// A shell or network call: needs a prompt that no grant, and no posture
    /// that cannot open one, may satisfy.
    ForcePrompt,
}

/// The approval an extension's call of `name` needs. `planned_requires_approval`
/// is what planning decided without regard to origin (a read outside the
/// workspace, a hook's ask or repo law all say yes there, and stay yes).
pub(crate) fn origin_approval(
    name: &str,
    input: &Value,
    planned_requires_approval: bool,
    spec: Option<&dyn ToolSpec>,
) -> OriginApproval {
    use crate::tools::execution_envelope::{CallClass, classify_call};

    let lower = name.to_ascii_lowercase();
    let resolved = canonical(name, input);
    let forced = |candidate: &str| {
        matches!(
            get_tool_category_for_call(candidate, input),
            ToolCategory::Shell | ToolCategory::Network
        ) || FORCED_PROMPT_NAMES.contains(&candidate)
    };
    // Names cover core meta-tools; the registered capability and concrete
    // execution classification also cover tools added without a name-list row.
    let reaches_or_executes = spec.is_some_and(|spec| {
        spec.capabilities().contains(&ToolCapability::Network)
            || matches!(
                classify_call(spec.name(), input, spec),
                CallClass::VerificationFilter
                    | CallClass::UnboundedVerification
                    | CallClass::BoundedFetch
                    | CallClass::Executes
                    | CallClass::Reaches
            )
    });
    if forced(&lower) || forced(&resolved) || reaches_or_executes {
        OriginApproval::ForcePrompt
    } else if EXT_AUTO_ELIGIBLE.contains(&resolved.as_str()) && !planned_requires_approval {
        OriginApproval::Unchanged
    } else {
        OriginApproval::Prompt
    }
}

/// One extension tool call's standing to ask the core for things: its ticket
/// and the machinery the asks run on. Lives from the `tool/call`'s start to
/// its end.
pub(crate) struct Invocation {
    tier: HostTier,
    host_generation: u64,
    owner: OwnerRef,
    scope: Option<codewhale_config::EntryRef>,
    plugins: Option<Arc<crate::plugins::PluginRegistry>>,
    content_hash: String,
    calls: CodemodeInvoker,
    gate: NestedCallGate,
    root_caller: Option<SessionCheckpointCaller>,
    tickets: Vec<Ticket>,
    session_call_lock: tokio::sync::Mutex<()>,
    /// Fires when the invocation ends or is revoked: withdraws every
    /// `core/call` still waiting or running for it.
    cancel: CancellationToken,
}

/// The tickets and invocations of one manager.
#[derive(Default)]
pub(crate) struct CoreCalls {
    pub(super) tickets: super::ticket::TicketTable,
    invocations: Mutex<HashMap<String, Arc<Invocation>>>,
}

impl CoreCalls {
    /// Start an invocation for the `tool/call` `call_id` of `owner`'s tool
    /// running on `tier`'s host process `host_generation`, if `gate` is the
    /// nested-call gate the turn loop serves for exactly this tool
    /// (`expected`). `None` otherwise: no ticket, so no `core/call`.
    #[cfg(test)]
    pub(crate) fn begin(
        self: &Arc<Self>,
        tier: HostTier,
        host_generation: u64,
        owner: &OwnerRef,
        call_id: &str,
        expected: &ExtensionCaller,
        context: &ToolContext,
        gate: &NestedCallGate,
    ) -> Option<InvocationGuard> {
        self.begin_scoped(
            tier,
            host_generation,
            owner,
            call_id,
            expected,
            context,
            gate,
            None,
            String::new(),
        )
    }
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn begin_scoped(
        self: &Arc<Self>,
        tier: HostTier,
        host_generation: u64,
        owner: &OwnerRef,
        call_id: &str,
        expected: &ExtensionCaller,
        context: &ToolContext,
        gate: &NestedCallGate,
        scope: Option<codewhale_config::EntryRef>,
        content_hash: String,
    ) -> Option<InvocationGuard> {
        let (caller, specs) = gate.extension()?;
        if caller != expected {
            return None;
        }
        let calls = CodemodeInvoker::for_extension(
            specs.to_vec(),
            context.clone(),
            gate.clone(),
            call_id.to_string(),
            refusal,
        );
        let ticket = self.tickets.mint(Grant {
            kind: TicketKind::Invocation,
            tier,
            host_generation,
            owner: owner.clone(),
            method: METHOD,
            target: json!({ "call_id": call_id }),
            ttl: TICKET_TTL,
            uses: MAX_CALLS_PER_INVOCATION,
        });
        let root_caller = context
            .execution
            .session_objects
            .as_ref()
            .filter(|session| !session.session_id.is_empty())
            .zip(
                context
                    .execution
                    .origin_turn_id
                    .as_ref()
                    .filter(|id| !id.is_empty()),
            )
            .filter(|_| context.execution.owner_agent_id.is_none())
            .map(|(session, turn_id)| SessionCheckpointCaller {
                session_id: session.session_id.clone(),
                agent_id: None,
                turn_id: turn_id.clone(),
                call_id: call_id.to_string(),
                workspace: context.workspace.clone(),
            });
        let checkpoint_ticket = root_caller.as_ref().map(|_| {
            self.tickets.mint(Grant {
                kind: TicketKind::Invocation,
                tier,
                host_generation,
                owner: owner.clone(),
                method: CHECKPOINT_METHOD,
                target: json!({ "call_id": call_id }),
                ttl: TICKET_TTL,
                uses: MAX_CALLS_PER_INVOCATION,
            })
        });
        let question_ticket = root_caller.as_ref().map(|_| {
            self.tickets.mint(Grant {
                kind: TicketKind::Invocation,
                tier,
                host_generation,
                owner: owner.clone(),
                method: QUESTION_METHOD,
                target: json!({ "call_id": call_id }),
                ttl: TICKET_TTL,
                uses: MAX_CALLS_PER_INVOCATION,
            })
        });
        let invocation = Arc::new(Invocation {
            tier,
            host_generation,
            owner: owner.clone(),
            scope,
            plugins: context.plugin_registry.clone(),
            content_hash,
            calls,
            gate: gate.clone(),
            root_caller,
            tickets: std::iter::once(ticket.clone())
                .chain(checkpoint_ticket.iter().cloned())
                .chain(question_ticket.iter().cloned())
                .collect(),
            session_call_lock: tokio::sync::Mutex::new(()),
            cancel: CancellationToken::new(),
        });
        self.invocations
            .lock()
            .expect("invocations lock")
            .insert(ticket.expose().to_string(), Arc::clone(&invocation));
        if let Some(checkpoint_ticket) = checkpoint_ticket.as_ref() {
            self.invocations.lock().expect("invocations lock").insert(
                checkpoint_ticket.expose().to_string(),
                Arc::clone(&invocation),
            );
        }
        if let Some(question_ticket) = question_ticket.as_ref() {
            self.invocations.lock().expect("invocations lock").insert(
                question_ticket.expose().to_string(),
                Arc::clone(&invocation),
            );
        }
        Some(InvocationGuard {
            question_ticket,
            checkpoint_ticket,
            core_calls: Arc::clone(self),
            ticket,
            invocation,
        })
    }

    /// The invocation `ticket` belongs to, still live.
    fn invocation(&self, ticket: &str) -> Option<Arc<Invocation>> {
        self.invocations
            .lock()
            .expect("invocations lock")
            .get(ticket)
            .cloned()
    }

    /// Revoke and withdraw whatever `keep` says to drop.
    fn revoke_where(&self, drop_it: impl Fn(&Invocation) -> bool) {
        let gone: Vec<Arc<Invocation>> = {
            let mut invocations = self.invocations.lock().expect("invocations lock");
            let keys: Vec<String> = invocations
                .iter()
                .filter(|(_, invocation)| drop_it(invocation))
                .map(|(key, _)| key.clone())
                .collect();
            keys.iter()
                .filter_map(|key| invocations.remove(key))
                .collect()
        };
        for invocation in gone {
            for ticket in &invocation.tickets {
                self.tickets.revoke(ticket);
            }
            invocation.cancel.cancel();
        }
    }

    /// `plugin_id`'s owner was revoked: every ticket it holds is revoked and
    /// every core call it has pending is withdrawn.
    pub(crate) fn revoke_owner(&self, plugin_id: &str) {
        self.tickets.revoke_owner(plugin_id);
        self.revoke_where(|invocation| invocation.owner.plugin_id == plugin_id);
    }

    pub(crate) fn revoke_scope(&self, plugin_id: &str, scope: &codewhale_config::EntryRef) {
        self.revoke_where(|invocation| {
            invocation.owner.plugin_id == plugin_id && invocation.scope.as_ref() == Some(scope)
        });
    }

    pub(crate) fn revoke_attachment(&self, id: u64) {
        self.revoke_where(|invocation| {
            invocation
                .plugins
                .as_ref()
                .and_then(|plugins| plugins.caller_selection())
                .is_some_and(|selection| selection.attachment_id == id)
        });
    }

    /// One host process exited.
    pub(crate) fn revoke_host(&self, tier: HostTier, host_generation: u64) {
        self.tickets.revoke_host(tier, host_generation);
        self.revoke_where(|invocation| {
            invocation.tier == tier && invocation.host_generation == host_generation
        });
    }

    /// How many tickets are live.
    #[cfg(test)]
    pub(crate) fn live_tickets(&self) -> usize {
        self.tickets.live()
    }

    fn end(&self, ticket: &Ticket) {
        if let Some(invocation) = self.invocation(ticket.expose()) {
            self.revoke_where(|candidate| std::ptr::eq(candidate, invocation.as_ref()));
        } else {
            self.tickets.revoke(ticket);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn redeem_invocation(
        &self,
        shared: &ManagerShared,
        tier: HostTier,
        host_generation: u64,
        owner: &OwnerRef,
        ticket: &str,
        method: &str,
        cx: &HostRequestContext,
    ) -> Result<Arc<Invocation>, RpcErrorWire> {
        let refuse = |code, message| RpcErrorWire {
            code,
            message,
            data: None,
        };
        let redeemed = self.tickets.redeem(&Presented {
            ticket,
            kind: TicketKind::Invocation,
            tier,
            host_generation,
            owner,
            method,
            target: None,
        });
        if let Err(refused) = redeemed {
            if refused.violation {
                cx.violation(format!(
                    "too many invalid {method} tickets from one host process"
                ));
            }
            return Err(refuse(
                error_code::REFUSED,
                refused.reason.describe().to_string(),
            ));
        }
        // The redemption proved the ticket is live and ours; the owner must
        // still be the current one of this tier.
        let live = shared
            .registry
            .lock()
            .expect("registry lock")
            .tier_of(owner)
            == Some(tier);
        let Some(invocation) = self.invocation(ticket).filter(|_| live) else {
            return Err(refuse(
                error_code::REFUSED,
                format!("the invocation this {method} belongs to has ended"),
            ));
        };
        shared
            .check_selection(
                invocation
                    .plugins
                    .as_ref()
                    .and_then(|plugins| plugins.caller_selection()),
                invocation.plugins.as_deref(),
                &owner.plugin_id,
                &invocation.content_hash,
                invocation.scope.as_ref(),
            )
            .map_err(|message| refuse(error_code::REFUSED, message))?;
        if invocation.cancel.is_cancelled() {
            return Err(refuse(
                error_code::REFUSED,
                "the invocation has ended".to_string(),
            ));
        }
        Ok(invocation)
    }

    pub(crate) async fn serve_checkpoint(
        &self,
        shared: &ManagerShared,
        tier: HostTier,
        host_generation: u64,
        params: SessionCheckpointParams,
        cx: HostRequestContext,
    ) -> Result<Value, RpcErrorWire> {
        let refuse = |code, message| RpcErrorWire {
            code,
            message,
            data: None,
        };
        let invocation = self.redeem_invocation(
            shared,
            tier,
            host_generation,
            &params.owner,
            &params.ticket,
            CHECKPOINT_METHOD,
            &cx,
        )?;
        let caller = invocation.root_caller.clone().ok_or_else(|| {
            refuse(
                error_code::NOT_AVAILABLE,
                "session checkpoint requires a live root invocation".into(),
            )
        })?;
        let withdraw = invocation.cancel.child_token();
        let _withdraw_guard = withdraw.clone().drop_guard();
        let outcome = tokio::select! {
            biased;
            () = cx.cancel.cancelled() => Err(crate::tools::spec::ToolError::cancelled("session checkpoint was cancelled by its host")),
            () = withdraw.cancelled() => Err(crate::tools::spec::ToolError::cancelled("session checkpoint invocation ended")),
            result = async {
                let _session_call = invocation.session_call_lock.lock().await;
                invocation.gate.checkpoint(caller, withdraw.clone()).await
            } => result,
        };
        if cx.cancel.is_cancelled() || invocation.cancel.is_cancelled() {
            return Err(refuse(
                error_code::CANCELLED,
                "session checkpoint invocation was withdrawn".into(),
            ));
        }
        if shared
            .registry
            .lock()
            .expect("registry lock")
            .tier_of(&params.owner)
            != Some(tier)
        {
            return Err(refuse(
                error_code::REFUSED,
                "session checkpoint owner is no longer current".into(),
            ));
        }
        shared
            .check_selection(
                invocation
                    .plugins
                    .as_ref()
                    .and_then(|plugins| plugins.caller_selection()),
                invocation.plugins.as_deref(),
                &params.owner.plugin_id,
                &invocation.content_hash,
                invocation.scope.as_ref(),
            )
            .map_err(|message| refuse(error_code::REFUSED, message))?;
        outcome.map(Value::Bool).map_err(|error| {
            let code = if matches!(&error, crate::tools::spec::ToolError::Cancelled { .. }) {
                error_code::CANCELLED
            } else {
                error_code::NOT_AVAILABLE
            };
            refuse(code, error.to_string())
        })
    }

    pub(crate) async fn serve_question(
        &self,
        shared: &ManagerShared,
        tier: HostTier,
        host_generation: u64,
        params: UserQuestionParams,
        cx: HostRequestContext,
    ) -> Result<Value, RpcErrorWire> {
        let refuse = |code, message| RpcErrorWire {
            code,
            message,
            data: None,
        };
        let invocation = self.redeem_invocation(
            shared,
            tier,
            host_generation,
            &params.owner,
            &params.ticket,
            QUESTION_METHOD,
            &cx,
        )?;
        let caller = invocation.root_caller.clone().ok_or_else(|| {
            refuse(
                error_code::NOT_AVAILABLE,
                "user question requires a live root invocation".into(),
            )
        })?;
        let request = crate::tools::user_input::UserInputRequest {
            require_typed_answers: true,
            questions: params
                .questions
                .into_iter()
                .map(|mut question| {
                    question.allow_free_text = true;
                    question
                })
                .collect(),
        };
        request
            .validate_transport()
            .map_err(|error| refuse(error_code::INVALID_PARAMS, error.to_string()))?;
        let withdraw = invocation.cancel.child_token();
        let _withdraw_guard = withdraw.clone().drop_guard();
        let outcome = tokio::select! {
            biased;
            () = cx.cancel.cancelled() => Err(crate::tools::spec::ToolError::cancelled("user question was cancelled by its host")),
            () = withdraw.cancelled() => Err(crate::tools::spec::ToolError::cancelled("user question invocation ended")),
            result = async {
                let _session_call = invocation.session_call_lock.lock().await;
                invocation.gate.user_question(caller, request, withdraw.clone(), invocation.calls.clock()).await
            } => result,
        };
        if cx.cancel.is_cancelled() || invocation.cancel.is_cancelled() {
            return Err(refuse(
                error_code::CANCELLED,
                "user question invocation was withdrawn".into(),
            ));
        }
        if shared
            .registry
            .lock()
            .expect("registry lock")
            .tier_of(&params.owner)
            != Some(tier)
        {
            return Err(refuse(
                error_code::REFUSED,
                "user question owner is no longer current".into(),
            ));
        }
        shared
            .check_selection(
                invocation
                    .plugins
                    .as_ref()
                    .and_then(|plugins| plugins.caller_selection()),
                invocation.plugins.as_deref(),
                &params.owner.plugin_id,
                &invocation.content_hash,
                invocation.scope.as_ref(),
            )
            .map_err(|message| refuse(error_code::REFUSED, message))?;
        outcome
            .map(|response| serde_json::to_value(response).expect("user input response serializes"))
            .map_err(|error| {
                let question_code = error
                    .metadata()
                    .and_then(|metadata| metadata.get("user_question_error"))
                    .and_then(Value::as_str)
                    .filter(|code| matches!(*code, "NO_PROVIDER" | "ASK_CANCELLED"));
                let code = if matches!(&error, crate::tools::spec::ToolError::Cancelled { .. })
                    || question_code == Some("ASK_CANCELLED")
                {
                    error_code::CANCELLED
                } else {
                    error_code::NOT_AVAILABLE
                };
                let data = question_code.map(|code| json!({"user_question_error": code}));
                RpcErrorWire {
                    code,
                    message: error.to_string(),
                    data,
                }
            })
    }

    /// Redeem `params` presented by `tier`'s host process `host_generation`
    /// and run the call. An invalid presentation is a refusal, and a burst of
    /// them a violation `cx` reports.
    pub(crate) async fn serve(
        &self,
        shared: &ManagerShared,
        tier: HostTier,
        host_generation: u64,
        params: CoreCallParams,
        cx: HostRequestContext,
    ) -> Result<Value, RpcErrorWire> {
        let refuse = |code: i64, message: String| RpcErrorWire {
            code,
            message,
            data: None,
        };
        let invocation = self.redeem_invocation(
            shared,
            tier,
            host_generation,
            &params.owner,
            &params.ticket,
            METHOD,
            &cx,
        )?;
        // Withdrawn when the host cancels this request, its owner is revoked,
        // the host exits (`cx.cancel`), or the invocation ends.
        let withdraw = invocation.cancel.child_token();
        {
            let (withdraw, host) = (withdraw.clone(), cx.cancel.clone());
            tokio::spawn(async move {
                tokio::select! {
                    () = host.cancelled() => withdraw.cancel(),
                    () = withdraw.cancelled() => {}
                }
            });
        }
        let outcome = invocation
            .calls
            .call(params.name, params.input, Some(&withdraw))
            .await;
        shared
            .check_selection(
                invocation
                    .plugins
                    .as_ref()
                    .and_then(|plugins| plugins.caller_selection()),
                invocation.plugins.as_deref(),
                &params.owner.plugin_id,
                &invocation.content_hash,
                invocation.scope.as_ref(),
            )
            .map_err(|message| refuse(error_code::REFUSED, message))?;
        let result = match outcome {
            Ok(response) => Ok(wire_from_response(response)),
            Err(NestedFailure::Rejected { decision, message }) => Err(refuse(
                if decision == NestedDecision::Denied {
                    error_code::DENIED
                } else {
                    error_code::REFUSED
                },
                message,
            )),
            Err(NestedFailure::Unavailable(message)) => Err(refuse(
                if withdraw.is_cancelled() {
                    error_code::CANCELLED
                } else {
                    error_code::NOT_AVAILABLE
                },
                message,
            )),
        };
        // Stop the forwarder when the call is over.
        withdraw.cancel();
        result.map(|wire| serde_json::to_value(wire).expect("wire results serialize"))
    }
}

/// An extension tool's answer to its `core/call`: the tool's text (and its
/// structured JSON when the content was JSON), bounded as code mode bounds it.
pub(crate) fn wire_from_response(response: ToolCallResponse) -> ToolResultWire {
    if !response.ok {
        let text = match response.result {
            Value::String(text) => text,
            other => other.to_string(),
        };
        return ToolResultWire {
            content: vec![ContentBlockWire::Text { text }],
            is_error: true,
            structured: None,
        };
    }
    let content = response
        .result
        .get("content")
        .cloned()
        .unwrap_or(Value::Null);
    let (mut text, structured) = match content {
        Value::String(text) => (text, None),
        other => (other.to_string(), Some(other)),
    };
    if let Some(cut) = response
        .result
        .get("truncated")
        .filter(|truncated| !truncated.is_null())
    {
        let number = |key: &str| cut.get(key).and_then(Value::as_u64).unwrap_or(0);
        text.push_str(&format!(
            "\n[output truncated: {} bytes in all, the first {} are shown]",
            number("original_bytes"),
            number("kept_bytes")
        ));
    }
    ToolResultWire {
        content: vec![ContentBlockWire::Text { text }],
        is_error: false,
        structured,
    }
}

/// Holds an invocation open. Dropping it (the `tool/call` ended or its future
/// was dropped) revokes its tickets and withdraws whatever is still pending.
pub(crate) struct InvocationGuard {
    core_calls: Arc<CoreCalls>,
    ticket: Ticket,
    checkpoint_ticket: Option<Ticket>,
    question_ticket: Option<Ticket>,
    invocation: Arc<Invocation>,
}

impl InvocationGuard {
    /// The ticket id to put in this call's `tool/call` (and nowhere else).
    pub(crate) fn ticket(&self) -> &str {
        self.ticket.expose()
    }

    pub(crate) fn checkpoint_ticket(&self) -> Option<&str> {
        self.checkpoint_ticket.as_ref().map(Ticket::expose)
    }

    pub(crate) fn question_ticket(&self) -> Option<&str> {
        self.question_ticket.as_ref().map(Ticket::expose)
    }

    /// The clock the `tool/call` deadline runs on: paused while one of this
    /// invocation's core calls waits on the gate.
    pub(crate) fn clock(&self) -> Arc<Mutex<PauseClock>> {
        self.invocation.calls.clock()
    }

    /// The receipts of the core calls made so far, bounded, for the tool's
    /// result metadata; `None` when it made none.
    pub(crate) fn receipts(&self) -> Option<Value> {
        let receipts = self.invocation.calls.receipts_json(RECEIPTS_ATTACHED);
        (receipts["total"].as_u64().unwrap_or(0) > 0).then_some(receipts)
    }
}

impl Drop for InvocationGuard {
    fn drop(&mut self) {
        self.core_calls.end(&self.ticket);
    }
}

//! Captured child authority projected onto the canonical Engine registry.
//! This module owns no model loop, tool registry, approval decision or history.
use super::*;

/// Captured billing origin and the existing worker projection only. This
/// carries no ChildGrant, tool/config authority, job, kernel or new ledger.
#[derive(Clone)]
pub(crate) struct ChildAccountingProjection {
    origin: SubAgentAccountingOrigin,
    runtime_usage_lease: Option<crate::cost_status::RuntimeUsageLease>,
    manager: SharedSubAgentManager,
    mailbox: Option<Mailbox>,
    owner: String,
}
impl ChildAccountingProjection {
    pub(super) fn capture(runtime: &SubAgentRuntime, owner: &str) -> Self {
        Self {
            origin: runtime.accounting_origin.clone(),
            runtime_usage_lease: runtime.runtime_usage_lease.clone(),
            manager: runtime.manager.clone(),
            mailbox: runtime.mailbox.clone(),
            owner: owner.to_owned(),
        }
    }
    pub(crate) fn publish(
        &self,
        source: &str,
        route: &crate::cost_status::EffectiveRouteEnvelope,
        usage: &Usage,
        reason: crate::cost_status::RuntimeUsageMissingReason,
    ) -> Option<u64> {
        let known = usage_has_reported_data(usage);
        let owner = self
            .runtime_usage_lease
            .as_ref()
            .map(crate::cost_status::RuntimeUsageLease::owner);
        if known {
            match owner {
                Some(owner) => crate::cost_status::report_effective_route_for_runtime(
                    self.origin.cost_scope,
                    Some(owner),
                    source,
                    route,
                    usage,
                ),
                None => self
                    .origin
                    .report_ownerless_usage(&self.owner, source, route, usage),
            }
            if let Some(mailbox) = &self.mailbox {
                let _ = mailbox.send(MailboxMessage::token_usage(
                    &self.owner,
                    source,
                    route.clone(),
                    usage.clone(),
                ));
            }
        } else {
            match owner {
                Some(owner) => crate::cost_status::report_missing_runtime_usage(
                    self.origin.cost_scope,
                    Some(owner),
                    source,
                    route,
                    reason,
                ),
                None => crate::cost_status::report_missing_usage_for_interactive_origin(
                    self.origin.cost_scope,
                    &self.origin.session_id,
                    &self.owner,
                    source,
                    route,
                    reason,
                ),
            }
        }
        known
            .then(|| priced_usd_microusd(&route.audit(usage)))
            .flatten()
    }
    /// Finish only the worker projection of an already-published receipt.
    /// The caller supplies its held Engine scheduler; no runtime is created.
    pub(crate) fn recover_settled(
        &self,
        scheduler: &tokio::runtime::Handle,
        source: String,
        route: crate::cost_status::EffectiveRouteEnvelope,
        usage: Usage,
        priced: Option<u64>,
        reason: crate::cost_status::RuntimeUsageMissingReason,
    ) {
        let manager = self.manager.clone();
        let owner = self.owner.clone();
        scheduler.spawn(async move {
            Self::project_worker_receipt(&manager, &owner, &source, &route, &usage, priced, reason)
                .await;
        });
    }
    pub(crate) async fn project_settled(
        &self,
        source: &str,
        route: &crate::cost_status::EffectiveRouteEnvelope,
        usage: &Usage,
        priced: Option<u64>,
        reason: crate::cost_status::RuntimeUsageMissingReason,
    ) {
        Self::project_worker_receipt(
            &self.manager,
            &self.owner,
            source,
            route,
            usage,
            priced,
            reason,
        )
        .await;
    }
    async fn project_worker_receipt(
        manager: &SharedSubAgentManager,
        owner: &str,
        source: &str,
        route: &crate::cost_status::EffectiveRouteEnvelope,
        usage: &Usage,
        priced: Option<u64>,
        reason: crate::cost_status::RuntimeUsageMissingReason,
    ) {
        let mut manager = manager.write().await;
        if usage_has_reported_data(usage) {
            manager.record_worker_routed_usage(owner, source, route, usage, priced);
        } else {
            manager.record_worker_missing_usage(
                owner,
                source,
                crate::cost_status::MissingUsageCoverage::for_route(route, reason),
            );
        }
    }
}

#[derive(Clone)]
pub(crate) struct ChildAuthority {
    pub(crate) grant: crate::worker_profile::ChildGrant,
    pub(super) disallowed_tools: Vec<String>,
    pub(super) accept_edits: bool,
    pub(crate) agent_type: FleetRole,
    pub(crate) owner_agent_id: String,
    pub(crate) owner_agent_name: String,
    pub(super) coordination_manager: SharedSubAgentManager,
    pub(super) enforce_write_claim: bool,
    pub(crate) runtime: SubAgentRuntime,
    pub(crate) person_wait: Arc<PersonWaitClock>,
}
impl std::fmt::Debug for ChildAuthority {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChildAuthority")
            .field("owner", &self.owner_agent_id)
            .field("grant", &self.grant)
            .finish_non_exhaustive()
    }
}
impl ChildAuthority {
    const ACTION_ALIASES: &'static [(&'static str, &'static str, &'static str)] =
        CANONICAL_ACTION_ALIASES;
    pub(crate) fn capture(
        runtime: SubAgentRuntime,
        agent_type: FleetRole,
        owner_agent_id: String,
        owner_agent_name: String,
        explicit_allowed_tools: Option<Vec<String>>,
    ) -> Arc<Self> {
        let parent_shell = if tool_denied(Some(&runtime.context.disallowed_tools), "bash") {
            ShellPolicy::None
        } else {
            ShellPolicy::from_legacy_allow_shell(runtime.allow_shell)
        };
        let mut effective = runtime.worker_profile.clone();
        effective.shell = effective.shell.min_with(parent_shell);
        let scope = intersect_explicit_tool_scope(&effective.tools, explicit_allowed_tools);
        let grant = crate::worker_profile::ChildGrant::resolve(
            &agent_type,
            &effective,
            scope,
            !runtime.would_exceed_depth(),
        );
        Arc::new(Self {
            grant,
            disallowed_tools: effective.denied_tools,
            accept_edits: runtime.accept_edits,
            agent_type,
            owner_agent_id,
            owner_agent_name,
            coordination_manager: runtime.manager.clone(),
            enforce_write_claim: true,
            runtime,
            person_wait: Arc::new(PersonWaitClock::default()),
        })
    }
    /// One child builder used by Core's actual turn registry. The supplied
    /// runtime already carries the exact turn route and nested completion inbox.
    pub(crate) fn tool_registry_builder(
        &self,
        runtime: SubAgentRuntime,
        todos: SharedTodoList,
        plan: SharedPlanState,
    ) -> ToolRegistryBuilder {
        let mut builder = ToolRegistryBuilder::new().with_full_agent_surface_options(
            Some(runtime.client.clone()),
            runtime.model.clone(),
            runtime.manager.clone(),
            runtime.clone(),
            runtime.agent_tool_surface_options.clone(),
            todos,
            plan,
        );
        if let Some(pool) = runtime.mcp_pool.as_ref() {
            builder = builder.with_mcp_tools(pool.clone());
        }
        builder
    }
    pub(crate) fn accounting_projection(&self) -> ChildAccountingProjection {
        ChildAccountingProjection::capture(&self.runtime, &self.owner_agent_id)
    }
    pub(crate) fn accounting_origin(&self) -> (crate::cost_status::CostScopeToken, String, String) {
        (
            self.runtime.accounting_origin.cost_scope,
            self.runtime.accounting_origin.session_id.clone(),
            self.owner_agent_id.clone(),
        )
    }
    pub(crate) async fn settle_response(
        &self,
        source_id: &str,
        route: crate::cost_status::EffectiveRouteEnvelope,
        usage: &Usage,
    ) {
        record_provider_response_usage(
            &self.runtime,
            &self.owner_agent_id,
            source_id,
            route,
            usage,
        )
        .await;
    }
    pub(crate) async fn project_settled_response(
        &self,
        source_id: &str,
        route: &crate::cost_status::EffectiveRouteEnvelope,
        usage: &Usage,
    ) {
        let priced = usage_has_reported_data(usage)
            .then(|| priced_usd_microusd(&route.audit(usage)))
            .flatten();
        self.runtime
            .manager
            .write()
            .await
            .record_worker_routed_usage(&self.owner_agent_id, source_id, route, usage, priced);
    }
    pub(crate) fn new_execution_id(&self) -> String {
        new_child_execution_id(&self.owner_agent_id)
    }
    pub(crate) fn pause_person_wait(&self) -> PersonWaitPause {
        self.person_wait.pause()
    }
    pub(crate) fn nested_runtime(
        &self,
        context: ToolContext,
        completion: mpsc::Sender<SubAgentCompletion>,
        fork: SubAgentForkContext,
    ) -> SubAgentRuntime {
        let mut runtime = self.runtime.clone();
        runtime.context = context;
        runtime.parent_agent_id = Some(self.owner_agent_id.clone());
        runtime.worker_profile.shell = self.grant.shell_policy();
        runtime.agent_tool_surface_options.shell_policy = self.grant.shell_policy();
        runtime.parent_completion_tx = Some(completion);
        runtime.fork_context = Some(fork);
        runtime
    }
    pub(crate) fn typed_error(error: anyhow::Error) -> ToolError {
        match error.downcast::<ToolError>() {
            Ok(error) => error,
            Err(error) => ToolError::permission_denied(error.to_string()),
        }
    }
    pub(crate) fn validate_context(
        &self,
        context: &ToolContext,
    ) -> std::result::Result<(), ToolError> {
        if context.workspace != self.runtime.context.workspace
            || context.owner_agent_id.as_deref() != Some(self.owner_agent_id.as_str())
            || context.state_namespace != self.runtime.context.state_namespace
        {
            return Err(ToolError::permission_denied(
                "child caller identity or workspace changed before dispatch",
            ));
        }
        if context
            .cancel_token
            .as_ref()
            .is_some_and(CancellationToken::is_cancelled)
            || self.runtime.cancel_token.is_cancelled()
        {
            return Err(ToolError::cancelled("child dispatch cancelled"));
        }
        Ok(())
    }
    pub(crate) async fn run_tool_bounded<F, T>(
        &self,
        future: F,
    ) -> std::result::Result<T, ToolError>
    where
        F: std::future::Future<Output = std::result::Result<T, ToolError>>,
    {
        let (deadline, _) = budget_handback::wall_deadlines(&self.runtime);
        match run_tool_with_person_aware_timeout(
            self.runtime.tool_timeout,
            deadline,
            &self.person_wait,
            future,
        )
        .await
        {
            Some(result) => result,
            None => Err(ToolError::execution_failed(
                "child tool or original work deadline exhausted",
            )),
        }
    }
    pub(crate) fn approval_receipt_store(
        &self,
    ) -> std::result::Result<crate::approval_log::ApprovalReceiptStore, String> {
        self.runtime
            .approval_receipt_store
            .clone()
            .unwrap_or_else(|| Err("child caller has no captured approval receipt store".into()))
    }
    pub(crate) fn context(self: &Arc<Self>) -> ToolContext {
        let mut context = self
            .runtime
            .context
            .clone()
            .with_owner_agent(self.owner_agent_id.clone(), self.owner_agent_name.clone())
            .with_shell_policy(self.grant.shell_policy());
        context.disallowed_tools = self.disallowed_tools.clone();
        context.child_host = Some(self.clone());
        context
    }
    pub(crate) fn validate(
        &self,
        registry: &ToolRegistry,
        name: &str,
        input: &Value,
    ) -> Result<()> {
        if !self.grant.desktop && is_machine_control_tool(name) {
            return Err(admission_denied(format!(
                "[tool.family.denied] Desktop/computer-control tool `{name}` is not available to sub-agents. Run it in the parent session instead, or ask the user."
            )));
        }
        if self.grant_blocks_tool(registry, name) {
            return Err(admission_denied(format!(
                "Tool {name} is not available to this read-only worker because its process path does not share the hardened evidence boundary. Use read/search, classifier-bounded bash reads, or the verifier's bounded Run tool instead."
            )));
        }
        let action = input.get("action").and_then(Value::as_str);
        if self.grant.surface == crate::worker_profile::ToolSurface::Evidence
            && name == "Web"
            && !matches!(action, Some("search" | "fetch"))
        {
            return Err(admission_denied(
                "Tool Web is limited to search/fetch in the read-only evidence profile",
            ));
        }
        // Catalog shaping is not authority. `agent` clears both name-keyed
        // gates below by design, so the per-action gate has to be repeated
        // here or a hand-written call would reach an action the role's own
        // catalog withheld.
        if name == "agent"
            && matches!(parse_agent_tool_action(input), Ok(AgentToolAction::Claim))
            && !self.agent_action_permitted("claim")
        {
            return Err(admission_denied(format!(
                "agent action=claim widens an enforced write scope, and the Fleet role `{role}` has no write authority to widen. Use an `implement` or `general` role.",
                role = self.agent_type.as_str()
            )));
        }
        let family_action_allowed = if !Self::ACTION_ALIASES
            .iter()
            .any(|(family, _, _)| *family == name)
        {
            true
        } else if let Some(action) = action {
            self.is_action_allowed(name, action)
        } else {
            self.grant
                .scope
                .as_ref()
                .is_none_or(|list| list.iter().any(|allowed| allowed == name))
        };
        if !self.is_tool_allowed(name) || !family_action_allowed {
            return Err(admission_denied(format!(
                "Tool {name} not allowed for this sub-agent; report the blocked probe to the parent instead of working around it"
            )));
        }
        // #3217: authoritative per-role posture — read-only roles cannot mutate
        // and non-`Full`-shell roles cannot run shell, regardless of whether
        // the parent session is auto-approved. This closes the auto-approve
        // bypass where a read-only child could quietly write or shell out.
        if !self.posture_permits_tool(registry, name, Some(input)) {
            if self.allows_bounded_readonly_bash(name) {
                // #6015: the same rule text and next steps as the durable
                // authority and the executor, from the one classifier.
                let lane =
                    crate::tools::shell::readonly_enforced_lane_available(registry.context());
                return Err(admission_denied(
                    match crate::tools::shell::agent_readonly_bash_verdict(input) {
                        Err(rejection) => format!(
                            "{} (tool {name}, Fleet role `{role}`)",
                            crate::tools::shell::readonly_refusal(&rejection, lane),
                            role = self.agent_type.as_str(),
                        ),
                        Ok(()) => format!(
                            "[shell.readonly.command] Tool {name} input did not match the bounded read-only shell grammar for Fleet role `{role}`. {guidance}",
                            role = self.agent_type.as_str(),
                            guidance =
                                codewhale_execpolicy::command_safety::readonly_command_help()
                        ),
                    },
                ));
            }
            return Err(admission_denied(format!(
                "[role.posture.denied] Tool {name} is not permitted for the read-only Fleet role `{role}`. Use an `implement` or `general` role (or `custom` with an explicit allowed_tools list) to mutate the workspace or run shell commands.",
                role = self.agent_type.as_str()
            )));
        }
        // Denied network capability cannot be expanded by answering a prompt.
        if self.network_is_denied() {
            reject_network_reaching_input(name, input).map_err(Self::typed_error)?;
        }
        reject_subagent_terminal_takeover(name, input).map_err(Self::typed_error)?;
        if self.write_is_denied() {
            reject_unbounded_verification(name, input, !self.shell_is_denied())
                .map_err(Self::typed_error)?;
        }
        // The centralized envelope check. Everything above is name- or
        // shape-specific; this one is derived from the tool's real capabilities
        // and this call's canonical action, so it also covers the tools no list
        // in this file can name — repository plugins, runtime MCP server tools,
        // and anything registered later. The bounded read-only bash carve-out
        // (#5426/#5438) carries its proven-read-only evidence so the envelope
        // classifies it Bounded instead of refusing it as Executes.
        if let Some(spec) = registry.get(name) {
            crate::tools::execution_envelope::enforce_execution_envelope(
                name,
                input,
                spec.as_ref(),
                self.execution_envelope(),
                self.bounded_readonly_bash_evidence(name, input),
            )
            .map_err(admission_denied)?;
        }
        Ok(())
    }
    pub(crate) async fn validate_claim(
        &self,
        registry: &ToolRegistry,
        name: &str,
        input: &Value,
    ) -> Result<Vec<String>> {
        self.validate(registry, name, input)?;
        let scope_aware_write = matches!(
            name,
            "write" | "edit" | "write_file" | "edit_file" | "apply_patch" | "fim_edit"
        ) || (name == "File"
            && input
                .get("action")
                .and_then(Value::as_str)
                .is_some_and(|action| matches!(action, "write" | "edit" | "patch")))
            || (name == "pandoc_convert" && input.get("output_path").is_some());
        if scope_aware_write && self.enforce_write_claim {
            let paths = mutation_paths(name, input)?;
            if paths.is_empty() {
                return Err(admission_denied(format!(
                    "Write tool {name} did not expose a bounded repo-relative target for coordination"
                )));
            }
            let held = self.coordination_manager.clone().read_owned().await;
            let owner = self.owner_agent_id.clone();
            codewhale_app_server::daemon_socket::owner_work(move || {
                held.validate_write_scope(&owner, &paths)
                    .map_err(anyhow::Error::msg)
            })
            .await
            .map_err(|error| admission_denied(error.to_string()))?;
        } else if self.enforce_write_claim
            // The typed read-only boundary above already rejected mutation.
            && !self.write_is_denied()
            && !is_internal_coordination_state_tool(name)
            // A shell run the read-only classifier proves mutation-free cannot
            // collide with the peer's writes no matter how contended the
            // checkout is, so the gate below does not apply to it.
            && !proven_readonly_shell_run(name, input)
            && (is_unbounded_shell_run(name, input)
                || registry.get(name).is_some_and(|spec| {
                    let canonical = canonical_action_alias(name, input);
                    let is_shell_control = matches!(
                        canonical,
                        "exec_shell_wait" | "exec_shell_interact" | "exec_shell_cancel"
                    );
                    let capabilities = spec.capabilities();
                    !is_shell_control
                        && (spec.approval_requirement_for(input) == ApprovalRequirement::Suggest
                            || (!spec.is_read_only_for(input)
                                && capabilities.iter().any(|capability| {
                                    matches!(
                                        capability,
                                        ToolCapability::WritesFiles
                                            | ToolCapability::ExecutesCode
                                            | ToolCapability::Network
                                    )
                                })))
                }))
        {
            let held = self.coordination_manager.clone().read_owned().await;
            let owner = self.owner_agent_id.clone();
            let blocking_peers = codewhale_app_server::daemon_socket::owner_work(move || {
                // A peer-free checkout still requires the exact held origin.
                // The file/root checks stay on the bounded owner worker.
                if let Some(claim) = held.shared_write_claim(&owner) {
                    held.validate_coordination_claim_root(&owner, claim)
                        .map_err(anyhow::Error::msg)?;
                    Ok(held.live_peer_shared_write_claim_owners(&owner))
                } else {
                    Ok(Vec::new())
                }
            })
            .await
            .map_err(|error| admission_denied(error.to_string()))?;
            if !blocking_peers.is_empty() {
                return Err(admission_denied(format!(
                    "Tool {name} cannot prove a bounded file target or read-only execution while peers are writing in this shared checkout (blocking peers: {}). Use a bounded write tool, a proven read-only command, or bash with read_only=true for analysis under native enforcement. Executable work that needs writes requires worktree isolation. Disjoint write_roots alone do not constrain arbitrary code.",
                    blocking_peers.join(", ")
                )));
            }
        }
        let observed_paths = if scope_aware_write {
            mutation_paths(name, input)?
        } else {
            Vec::new()
        };
        Ok(observed_paths)
    }
    pub(crate) async fn record_settled_writes(&self, paths: Vec<String>) {
        if paths.is_empty() {
            return;
        }
        let mut manager = self.coordination_manager.write().await;
        if let Some(record) = manager.worker_records.get_mut(&self.owner_agent_id) {
            record.delivery_evidence.observed_writes.extend(paths);
        }
    }
    pub(crate) fn delegated_call(
        &self,
        registry: &ToolRegistry,
        name: &str,
        input: &Value,
    ) -> bool {
        if self.bounded_readonly_bash_evidence(name, input) {
            return true;
        }
        registry
            .get(name)
            .is_some_and(|spec| match spec.approval_requirement_for(input) {
                ApprovalRequirement::Auto => true,
                ApprovalRequirement::Suggest => {
                    self.grant.files == crate::worker_profile::FileGrant::Write
                        && (self.accept_edits
                            || Self::role_can_delegate_writes(&self.agent_type)
                            || self.workspace_write_carve_out_permits(registry, name, input))
                }
                ApprovalRequirement::Required => {
                    Self::is_delegated_builtin_verification(name, input)
                }
            })
    }
    pub(crate) fn role_can_delegate_writes(agent_type: &FleetRole) -> bool {
        // Builder is the named implementation role. Custom may write only when
        // its profile, explicit scope, and execution envelope all agree.
        matches!(agent_type, FleetRole::Builder | FleetRole::Custom)
    }

    pub(crate) fn workspace_write_carve_out_permits(
        &self,
        registry: &ToolRegistry,
        name: &str,
        input: &Value,
    ) -> bool {
        // This is a bounded convenience for write-capable children, not an
        // authority escalation: every target must resolve inside the workspace
        // and still pass sensitive-path, repository-law, and claim checks.
        if self.grant.files != crate::worker_profile::FileGrant::Write {
            return false;
        }
        crate::core::authority::paths_within_workspace_write_carve_out(
            &registry.context().workspace,
            &raw_mutation_target_paths(name, input),
        )
    }

    pub(crate) fn is_delegated_builtin_verification(name: &str, input: &Value) -> bool {
        use crate::tools::execution_envelope::{VerificationBound, classify_verification};

        // Reuse the same classifier as the execution envelope. This prevents a
        // second, looser notion of "test command" from growing in this module.
        matches!(
            classify_verification(canonical_action_alias(name, input), input),
            Some(VerificationBound::Default | VerificationBound::Filter)
        )
    }

    pub(crate) fn posture_permits_tool(
        &self,
        registry: &ToolRegistry,
        name: &str,
        input: Option<&Value>,
    ) -> bool {
        // Delegation depth governs `agent`; write posture must not accidentally
        // suppress it or turn depth into a mutation permission.
        if name == "agent" {
            return true;
        }
        match registry.get(name) {
            Some(spec) => match input.map_or_else(
                || spec.approval_requirement(),
                |input| spec.approval_requirement_for(input),
            ) {
                ApprovalRequirement::Auto => true,
                ApprovalRequirement::Suggest => {
                    self.grant.files == crate::worker_profile::FileGrant::Write
                }
                ApprovalRequirement::Required => {
                    // An outbound read needs approval because its payload can
                    // disclose data. That hold does not grant shell/write
                    // authority; network/envelope and parent approval gates
                    // below independently decide whether this child may send it.
                    let capabilities = spec.capabilities();
                    if capabilities.contains(&ToolCapability::ReadOnly)
                        && capabilities.contains(&ToolCapability::Network)
                        && !capabilities.contains(&ToolCapability::ExecutesCode)
                        && !capabilities.contains(&ToolCapability::WritesFiles)
                    {
                        return true;
                    }

                    // #5426 acceptance point 1: the bounded read-only shell.
                    // `allows_bounded_readonly_bash` admits canonical `bash`
                    // to the inspection grant through the raw-shell deny
                    // list; a call the agent read-only classifier proves
                    // mutation-free is Auto-class evidence, not a held
                    // mutation, so the gate must not demand a full shell
                    // grant for it. Judged by the same predicate
                    // `BashTool::execute` enforces under
                    // `ShellPolicy::ReadOnly` (shell.rs), so this admission
                    // can never widen past the execute-time refusal — the
                    // first live dogfood against #5428 was denied all three
                    // canonical inspection commands here because the gate
                    // consulted only `Required` → `Full`.
                    if self.allows_bounded_readonly_bash(name)
                        && input.is_some_and(crate::tools::shell::agent_readonly_bash_input)
                    {
                        return true;
                    }
                    // `Verify` holds process-start authority for the bounded
                    // verification surface; the envelope below still refuses
                    // its ExecutesCode/WritesFiles calls by capability.
                    self.grant.shell >= crate::worker_profile::ShellGrant::Verify
                }
            },
            None => true,
        }
    }

    pub(crate) fn is_tool_denied(&self, name: &str) -> bool {
        // The shared matcher canonicalizes legacy/lowercase spellings before
        // applying exact or prefix rules. For example `exec_shell*` denies
        // `bash`, and `write_file*` denies `write`, in roots and children alike.
        tool_denied(Some(&self.disallowed_tools), name)
    }

    pub(crate) fn allows_bounded_readonly_bash(&self, name: &str) -> bool {
        name == "bash" && self.grant.shell == crate::worker_profile::ShellGrant::Inspect
    }

    pub(crate) fn legacy_action_alias(family: &str, action: &str) -> Option<&'static str> {
        Self::ACTION_ALIASES
            .iter()
            .find_map(|(candidate_family, candidate_action, alias)| {
                (*candidate_family == family && *candidate_action == action).then_some(*alias)
            })
    }

    pub(crate) fn is_action_allowed(&self, family: &str, action: &str) -> bool {
        let alias = Self::legacy_action_alias(family, action);
        // Read-only inspection keeps two deliberate evidence carve-outs:
        // classifier-bounded Bash reads and Web search/fetch. They bypass only
        // the coarse family sentinel; action, posture, and envelope checks still
        // reject mutation, arbitrary shell, and non-evidence Web actions.
        let bounded_readonly_bash = self.allows_bounded_readonly_bash(family) && action == "run";
        let web_readonly_action = family.eq_ignore_ascii_case("Web")
            && matches!(action, "search" | "fetch")
            && self.network_is_denied();
        if self.is_tool_denied(family)
            || !bounded_readonly_bash
                && !web_readonly_action
                && alias.is_some_and(|name| self.is_tool_denied(name))
        {
            return false;
        }
        match &self.grant.scope {
            None => true,
            Some(list) => {
                list.iter().any(|name| name.eq_ignore_ascii_case(family))
                    || alias.is_some_and(|alias| explicit_scope_permits(list, alias))
            }
        }
    }

    pub(crate) fn is_tool_allowed(&self, name: &str) -> bool {
        if name == "agent" && !self.grant.spawn {
            return false;
        }
        if self.is_tool_denied(name) && !self.allows_bounded_readonly_bash(name) {
            return false;
        }
        match &self.grant.scope {
            None => true,
            Some(list) => {
                explicit_scope_permits(list, name)
                    || Self::ACTION_ALIASES.iter().any(|(family, _, alias)| {
                        *family == name && list.iter().any(|allowed| allowed == alias)
                    })
            }
        }
    }

    pub(crate) fn grant_blocks_tool(&self, registry: &ToolRegistry, name: &str) -> bool {
        let lower = name.to_ascii_lowercase();
        // Desktop / machine-control is never in a child's grant.
        if !self.grant.desktop && is_machine_control_tool(name) {
            return true;
        }
        // Core's virtual search returns this child's filtered catalog; it is
        // not a registered process tool. Scope and deny checks still follow.
        let evidence_tool = crate::core::engine::tool_catalog::is_tool_search_tool(name)
            || crate::tools::registry::readonly_evidence_tool_name(name)
            || registry
                .get(name)
                .is_some_and(|tool| crate::tools::registry::readonly_evidence_tool(tool.as_ref()));
        // The Evidence surface admits only the hardened evidence tools and
        // `agent` (delegation).
        if self.grant.surface == crate::worker_profile::ToolSurface::Evidence
            && lower != "agent"
            && !evidence_tool
        {
            return true;
        }
        // Below a Full shell grant the raw process surface is gone — except
        // canonical `bash` under Inspect, which the read-only classifier
        // bounds at dispatch.
        let raw_shell = lower == "bash"
            || lower.starts_with("exec_shell")
            || matches!(
                lower.as_str(),
                "exec_wait" | "exec_interact" | "task_shell_start" | "task_shell_wait"
            )
            || lower.starts_with("terminal/");
        raw_shell
            && self.grant.shell < crate::worker_profile::ShellGrant::Full
            && !self.allows_bounded_readonly_bash(name)
    }

    pub(crate) fn network_is_denied(&self) -> bool {
        !self.grant.network
    }

    pub(crate) fn write_is_denied(&self) -> bool {
        self.grant.files != crate::worker_profile::FileGrant::Write
    }

    pub(crate) fn shell_is_denied(&self) -> bool {
        self.grant.shell < crate::worker_profile::ShellGrant::Verify
    }

    pub(crate) fn execution_envelope(&self) -> crate::tools::execution_envelope::ExecutionEnvelope {
        // The capability envelope is the grant's own projection: catalog and
        // dispatch derive it from the same fields, never from a deny-list
        // sentinel or a role re-mapping that could disagree with them.
        crate::tools::execution_envelope::ExecutionEnvelope {
            write: self.grant.files == crate::worker_profile::FileGrant::Write,
            network: self.grant.network,
            shell: !self.shell_is_denied(),
        }
    }

    pub(crate) fn envelope_permits(
        &self,
        registry: &ToolRegistry,
        name: &str,
        input: &Value,
    ) -> bool {
        let envelope = self.execution_envelope();
        if envelope.is_unrestricted() {
            return true;
        }
        match registry.get(name) {
            Some(spec) => crate::tools::execution_envelope::enforce_execution_envelope(
                name,
                input,
                spec.as_ref(),
                envelope,
                self.bounded_readonly_bash_evidence(name, input),
            )
            .is_ok(),
            None => true,
        }
    }

    pub(crate) fn bounded_readonly_bash_evidence(&self, name: &str, input: &Value) -> bool {
        self.allows_bounded_readonly_bash(name)
            && crate::tools::shell::agent_readonly_bash_input(input)
    }

    pub(crate) fn agent_action_permitted(&self, action: &str) -> bool {
        // `release` (#5906) carried the same `agents/coordinate` authority as
        // `claim` and is gated identically: a read-only role has no write
        // scope, so no contention refusal to remediate.
        if !matches!(action, "claim" | "release") {
            return true;
        }
        self.grant.files == crate::worker_profile::FileGrant::Write
            && !self.is_tool_denied("agents/coordinate")
    }

    pub(crate) fn visibility_representative_input(&self, name: &str) -> Option<Value> {
        // Visibility and dispatch consult the same capability guard. These
        // representative calls let a read-only bash schema survive catalog
        // shaping without treating an empty input as arbitrary shell authority.
        if self.grant.shell != crate::worker_profile::ShellGrant::Inspect {
            return None;
        }
        match name {
            "bash" => Some(json!({"command": "pwd"})),
            "Bash" => Some(json!({"action": "run", "command": "pwd"})),
            _ => None,
        }
    }

    pub(crate) fn tools_for_model(
        &self,
        registry: &ToolRegistry,
        agent_type: &FleetRole,
    ) -> Vec<Tool> {
        // Filter the full registry in deny-first order. These catalog filters
        // reduce accidental exposure, but are never the authority boundary:
        // execute() repeats role, scope, posture, envelope, and claim checks.
        let _ = agent_type;
        let api_tools = registry.to_api_tools();
        let filtered = match &self.grant.scope {
            None => api_tools,
            Some(list) => api_tools
                .into_iter()
                .filter(|tool| {
                    explicit_scope_permits(list, &tool.name)
                        || is_action_family(&tool.name)
                            && tool.input_schema["properties"]["action"]["enum"]
                                .as_array()
                                .is_some_and(|actions| {
                                    actions.iter().any(|action| {
                                        action.as_str().is_some_and(|action| {
                                            Self::legacy_action_alias(&tool.name, action)
                                                .is_some_and(|alias| {
                                                    list.iter().any(|n| n == alias)
                                                })
                                        })
                                    })
                                })
                })
                .collect::<Vec<_>>(),
        };
        let mut tools = filtered
            .into_iter()
            .filter(|tool| tool.name != "agent" || self.grant.spawn)
            .filter(|tool| {
                !self.is_tool_denied(&tool.name) || self.allows_bounded_readonly_bash(&tool.name)
            })
            .filter(|tool| !self.grant_blocks_tool(registry, &tool.name))
            .filter(|tool| {
                let representative = self.visibility_representative_input(&tool.name);
                tool.name == "File"
                    || self.posture_permits_tool(registry, &tool.name, representative.as_ref())
            })
            .filter(|tool| {
                if is_action_family(&tool.name) {
                    return true;
                }
                let representative = self
                    .visibility_representative_input(&tool.name)
                    .unwrap_or_else(|| json!({}));
                self.envelope_permits(registry, &tool.name, &representative)
            })
            .collect::<Vec<_>>();

        for tool in &mut tools {
            if !is_action_family(&tool.name) {
                continue;
            }
            // Indexing `["properties"]["action"]["enum"]` mutably would
            // fabricate an `"action": {"enum": null}` property on schemas
            // that have no action discriminator (the lowercase `bash`
            // command/timeout shape) — a phantom node that fails Moonshot
            // MFJS validation. Only shape enums that already exist.
            let Some(actions) = tool
                .input_schema
                .pointer_mut("/properties/action/enum")
                .and_then(serde_json::Value::as_array_mut)
            else {
                continue;
            };
            actions.retain(|action| {
                let Some(action) = action.as_str() else {
                    return false;
                };
                let posture_allows = tool.name != "File"
                    || self.grant.files == crate::worker_profile::FileGrant::Write
                    || matches!(action, "read" | "list" | "search_name" | "search_content");
                let evidence_action = self.grant.surface
                    != crate::worker_profile::ToolSurface::Evidence
                    || tool.name != "Web"
                    || matches!(action, "search" | "fetch");
                let mut representative = self
                    .visibility_representative_input(&tool.name)
                    .unwrap_or_else(|| json!({}));
                representative["action"] = json!(action);
                posture_allows
                    && evidence_action
                    && self.is_action_allowed(&tool.name, action)
                    && self.envelope_permits(registry, &tool.name, &representative)
            });
        }
        // `agent` is not a `CANONICAL_ACTION_ALIASES` family, so the pruner
        // above never reaches it — and it must not become one, because
        // `canonical_action_alias` feeds `execution_envelope`, where `agent`'s
        // `ExecutesCode` capability is deliberately reclassified `Bounded`.
        // Shape its enum explicitly instead.
        for tool in &mut tools {
            if tool.name != "agent" {
                continue;
            }
            let Some(actions) = tool
                .input_schema
                .pointer_mut("/properties/action/enum")
                .and_then(serde_json::Value::as_array_mut)
            else {
                continue;
            };
            actions.retain(|action| {
                action
                    .as_str()
                    .is_some_and(|action| self.agent_action_permitted(action))
            });
        }
        tools.retain(|tool| {
            tool.input_schema["properties"]["action"]["enum"]
                .as_array()
                .is_none_or(|actions| !actions.is_empty())
        });
        tools
    }
}

/// Worker metadata and its existing transcript projection. Core Session remains
/// the request/history owner; these snapshots are delivery/resume receipts.
pub(crate) struct ChildJob {
    pub(crate) authority: Arc<ChildAuthority>,
    pub(crate) assignment: SubAgentAssignment,
    pub(crate) started_at: Instant,
    pub(crate) max_steps: u32,
    pub(crate) work_max_steps: u32,
    pub(crate) work_deadline: Option<Instant>,
    pub(crate) hard_deadline: Option<Instant>,
    pub(crate) fork_context: bool,
    parking: Option<Arc<std::sync::atomic::AtomicBool>>,
    artifact: tokio::sync::Mutex<Option<SubAgentTranscriptArtifactWriter>>,
    scheduler: tokio::runtime::Handle,
    pub(crate) requests: std::sync::atomic::AtomicU32,
    logical_steps: std::sync::atomic::AtomicU32,
    response_received: std::sync::atomic::AtomicBool,
    selected_model: std::sync::Mutex<String>,
    pacing_sent: std::sync::atomic::AtomicBool,
    stop_reason: std::sync::Mutex<Option<String>>,
}
impl ChildJob {
    pub(crate) async fn admitted(
        authority: Arc<ChildAuthority>,
        assignment: SubAgentAssignment,
        started_at: Instant,
        max_steps: u32,
        fork_context: bool,
        parking: Option<Arc<std::sync::atomic::AtomicBool>>,
    ) -> Result<Arc<Self>> {
        let work_max_steps = if max_steps >= 2 {
            max_steps - 1
        } else {
            max_steps
        };
        let (work_deadline, hard_deadline) = budget_handback::wall_deadlines(&authority.runtime);
        let artifact = SubAgentTranscriptArtifactWriter::for_runtime(
            &authority.runtime,
            &authority.owner_agent_id,
        )
        .await?;
        let selected_model = authority.runtime.model.clone();
        Ok(Arc::new(Self {
            authority,
            assignment,
            started_at,
            max_steps,
            work_max_steps,
            work_deadline,
            hard_deadline,
            fork_context,
            parking,
            artifact: tokio::sync::Mutex::new(Some(artifact)),
            scheduler: tokio::runtime::Handle::try_current()
                .map_err(|_| anyhow!("child admission requires the existing Engine scheduler"))?,
            requests: std::sync::atomic::AtomicU32::new(0),
            logical_steps: std::sync::atomic::AtomicU32::new(0),
            response_received: std::sync::atomic::AtomicBool::new(false),
            selected_model: std::sync::Mutex::new(selected_model),
            pacing_sent: std::sync::atomic::AtomicBool::new(false),
            stop_reason: std::sync::Mutex::new(None),
        }))
    }
    pub(crate) async fn record_route_replacement(
        &self,
        current: &SubAgentRuntime,
        next: &SubAgentRuntime,
        source: SpawnRouteSource,
        note: String,
        error: &anyhow::Error,
    ) {
        let mut manager = self.authority.runtime.manager.write().await;
        if let Some(origin) = current.route_origin.as_deref() {
            manager.record_refused_route(
                &origin.route,
                &current.client.redact_model_bound_text(&error.to_string()),
            );
        }
        manager.record_route_replacement(&self.authority.owner_agent_id, next, source, note);
    }
    pub(crate) fn can_replace_first_request(&self) -> bool {
        self.steps() == 1
            && !self
                .response_received
                .load(std::sync::atomic::Ordering::Acquire)
            && !self.authority.runtime.cancel_token.is_cancelled()
    }
    pub(crate) fn installed_replacement(&self, model: &str) {
        self.logical_steps
            .store(0, std::sync::atomic::Ordering::Relaxed);
        *self
            .selected_model
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = model.to_owned();
    }
    pub(crate) fn pacing_notice(&self) -> Option<String> {
        if self.pacing_sent.load(std::sync::atomic::Ordering::Relaxed) {
            return None;
        }
        let notice = child_budget_pacing_notice(
            self.started_at,
            self.work_deadline,
            self.steps(),
            self.work_max_steps,
        )?;
        (!self
            .pacing_sent
            .swap(true, std::sync::atomic::Ordering::Relaxed))
        .then_some(notice)
    }
    pub(crate) async fn project(
        &self,
        messages: &[Message],
        steps: u32,
        system_prompt: Option<&SystemPrompt>,
    ) -> Result<()> {
        let mut artifact = self.artifact.lock().await;
        if let Some(writer) = artifact.as_mut() {
            if let Some(system) =
                system_prompt.filter(|system| writer.last_system_prompt.as_ref() != Some(*system))
            {
                let encoded = json_line(&json!({
                    "kind": "system_prompt", "at_message_index": writer.persisted_messages,
                    "system_prompt": system,
                }))?;
                let state_root = writer.state_root.clone();
                let path = writer.path.clone();
                tokio::task::spawn_blocking(move || {
                    append_private_subagent_transcript(&state_root, &path, &encoded, true)
                })
                .await
                .map_err(|error| anyhow!("child prompt receipt worker: {error}"))??;
                writer.last_system_prompt = Some(system.clone());
            }
            writer.sync_messages(messages, true)?;
        }
        let checkpoint = checkpoint_subagent_progress(
            &self.authority.runtime,
            &self.authority.owner_agent_id,
            "Core child Session checkpoint",
            messages,
            steps,
            true,
        )
        .await;
        publish_live_subagent_transcript(
            &self.authority.runtime,
            &self.authority.owner_agent_id,
            &self.authority.agent_type,
            &self.assignment,
            None,
            Some(&checkpoint),
            artifact.as_mut(),
            messages,
            steps,
            self.started_at,
            self.fork_context,
        )
        .await;
        Ok(())
    }
    pub(crate) async fn before_replace(&self, old: &[Message], new: &[Message]) -> Result<()> {
        let mut artifact = self.artifact.lock().await;
        if let Some(writer) = artifact.as_mut() {
            writer.sync_messages(old, true)?;
            writer.record_compaction(old, new)?;
            writer.append_messages(&[], true)?;
        }
        Ok(())
    }
    pub(crate) fn stop_for_budget(&self, reason: &str) {
        let mut slot = self
            .stop_reason
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        slot.get_or_insert_with(|| reason.to_owned());
    }
    pub(crate) fn budget_reason(&self) -> Option<String> {
        self.stop_reason
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
    pub(crate) fn steps(&self) -> u32 {
        self.logical_steps
            .load(std::sync::atomic::Ordering::Relaxed)
    }
    pub(crate) fn provider_refused(&self, error: &anyhow::Error) {
        if matches!(
            error.downcast_ref::<LlmError>(),
            Some(LlmError::RateLimited { .. })
        ) && let Some(governor) = self.authority.runtime.governor.as_ref()
        {
            governor.record_rate_limited(Instant::now());
        }
    }
    pub(crate) fn dispatched(
        self: &Arc<Self>,
        logical_step: u32,
        source: String,
        route: crate::cost_status::EffectiveRouteEnvelope,
    ) -> ChildDispatchedRequest {
        if let Some(governor) = self.authority.runtime.governor.as_ref() {
            governor.record_attempt(Instant::now());
        }
        self.logical_steps
            .fetch_max(logical_step, std::sync::atomic::Ordering::Relaxed);
        self.requests
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        ChildDispatchedRequest {
            job: self.clone(),
            source,
            route,
            settled: None,
            refused: false,
            projected: false,
        }
    }
    pub(crate) fn provider_responded(&self) {
        // Even an incomplete/failed decoder means the provider accepted this
        // request. It is never eligible for first-request route replacement.
        self.response_received
            .store(true, std::sync::atomic::Ordering::Release);
    }
    pub(crate) fn response_settled(&self, success: bool) {
        if success && let Some(governor) = self.authority.runtime.governor.as_ref() {
            governor.record_success(Instant::now());
        }
    }
    pub(crate) async fn finish(
        &self,
        messages: &[Message],
        steps: u32,
        status: SubAgentStatus,
        text: Option<String>,
        reason: Option<&str>,
    ) -> Result<SubAgentResult> {
        self.project(messages, steps, None).await?;
        let mut artifact = self.artifact.lock().await;
        if self.authority.runtime.cancel_token.is_cancelled() {
            // `project` has durably synced this exact Core Session and updated
            // the captured worker. Return that checkpoint rather than discarding
            // it while projecting the cancellation result.
            let checkpoint = self
                .authority
                .runtime
                .manager
                .read()
                .await
                .get_result(&self.authority.owner_agent_id)?
                .checkpoint;
            return Ok(cancelled_subagent_result(
                &self.authority.runtime,
                &self.authority.owner_agent_id,
                &self.authority.agent_type,
                &self.assignment,
                messages,
                steps,
                self.max_steps,
                checkpoint.as_ref(),
                self.parking.as_ref(),
                &mut artifact,
                self.started_at,
                self.fork_context,
                " in Core child turn",
            )
            .await);
        }
        let checkpoint = build_subagent_checkpoint(
            &self.authority.owner_agent_id,
            reason.unwrap_or_else(|| subagent_status_name(&status)),
            messages,
            steps,
            matches!(status, SubAgentStatus::Interrupted(_)),
        );
        let duration_ms = u64::try_from(self.started_at.elapsed().as_millis()).unwrap_or(u64::MAX);
        insert_subagent_full_transcript_handle(
            &self.authority.runtime,
            &self.authority.owner_agent_id,
            &self.authority.agent_type,
            &self.assignment,
            &status,
            text.as_ref(),
            Some(&checkpoint),
            artifact.as_mut(),
            messages,
            steps,
            duration_ms,
            self.fork_context,
        )
        .await;
        let mut result = self
            .authority
            .runtime
            .manager
            .read()
            .await
            .get_result(&self.authority.owner_agent_id)?;
        result.status = status;
        result.result = text;
        result.steps_taken = steps;
        result.checkpoint = Some(checkpoint);
        result.duration_ms = duration_ms;
        result.model = self
            .selected_model
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        result.needs_input = match &result.status {
            SubAgentStatus::Interrupted(reason) => result
                .checkpoint
                .as_ref()
                .map(|checkpoint| needs_input_for_interrupted_checkpoint(reason, checkpoint)),
            _ => None,
        };
        result.usage = self
            .authority
            .runtime
            .manager
            .read()
            .await
            .worker_records
            .get(&self.authority.owner_agent_id)
            .map(|record| record.usage.clone());
        if result.status == SubAgentStatus::BudgetExhausted {
            let cause = reason.unwrap_or("child work budget exhausted");
            if let Some(note) = budget_work_preservation_note(
                &self.authority.runtime.manager,
                &self.authority.owner_agent_id,
                cause,
            )
            .await
            {
                result
                    .result
                    .get_or_insert_with(String::new)
                    .push_str(&format!("\n\n{note}"));
            }
            let artifact = if let Some(body) = result.result.clone() {
                budget_handback::write_digest_artifact(
                    &self.authority.runtime,
                    &self.authority.owner_agent_id,
                    body,
                )
                .await
            } else {
                None
            };
            let mut handback_note = "The bounded Core hand-back preserves the recorded work; the assignment is not complete.".to_string();
            if let Some(artifact) = artifact {
                handback_note.push_str(&format!(
                    " Recorded work is saved as this child's deliverable: {}.",
                    artifact.display()
                ));
            }
            result = budget_partial_result_with_note(
                result,
                reason.unwrap_or("child work budget exhausted"),
                &handback_note,
            );
        }
        release_resident_leases_for(&self.authority.owner_agent_id);
        Ok(result)
    }
}

/// Exact primary dispatch ownership. Dropping an actually invoked request
/// records ambiguity under its captured origin; an unopened/queued turn never
/// constructs this guard. Billing is synchronous before worker projection.
pub(crate) struct ChildDispatchedRequest {
    job: Arc<ChildJob>,
    source: String,
    route: crate::cost_status::EffectiveRouteEnvelope,
    settled: Option<(
        Usage,
        Option<u64>,
        crate::cost_status::RuntimeUsageMissingReason,
    )>,
    refused: bool,
    projected: bool,
}
/// Only a typed authoritative rejection proves the dispatched operation
/// produced no billable response. Network/body/cancellation ambiguity does not.
pub(crate) fn provider_request_refusal_proven(error: &anyhow::Error) -> bool {
    matches!(
        error.downcast_ref::<LlmError>(),
        Some(
            LlmError::RateLimited { .. }
                | LlmError::QuotaExhausted(_)
                | LlmError::AuthenticationError(_)
                | LlmError::AuthorizationError(_)
                | LlmError::InvalidRequest { .. }
                | LlmError::ModelError(_)
                | LlmError::ContentPolicyError(_)
                | LlmError::ContextLengthError(_)
        )
    )
}
impl ChildDispatchedRequest {
    pub(crate) async fn settle_open_error(&mut self, error: &anyhow::Error) {
        self.refused = provider_request_refusal_proven(error);
        if !self.refused {
            self.settle(&Usage::default(), false).await;
        }
    }
    pub(crate) async fn settle(&mut self, usage: &Usage, complete: bool) {
        let reason = if complete {
            crate::cost_status::RuntimeUsageMissingReason::SuccessWithoutUsage
        } else {
            crate::cost_status::RuntimeUsageMissingReason::RequestOutcomeUnknown
        };
        let priced = report_provider_response_usage_origin(
            &self.job.authority.runtime,
            &self.job.authority.owner_agent_id,
            &self.source,
            &self.route,
            usage,
            reason,
        );
        self.settled = Some((usage.clone(), priced, reason));
        self.project(usage, priced, reason).await;
        self.projected = true;
    }
    async fn project(
        &self,
        usage: &Usage,
        priced: Option<u64>,
        reason: crate::cost_status::RuntimeUsageMissingReason,
    ) {
        self.job
            .authority
            .accounting_projection()
            .project_settled(&self.source, &self.route, usage, priced, reason)
            .await;
    }
}
impl Drop for ChildDispatchedRequest {
    fn drop(&mut self) {
        if self.refused || self.projected {
            return;
        }
        let (usage, priced, reason) = self.settled.clone().unwrap_or_else(|| {
            let reason = crate::cost_status::RuntimeUsageMissingReason::RequestOutcomeUnknown;
            report_provider_response_usage_origin(
                &self.job.authority.runtime,
                &self.job.authority.owner_agent_id,
                &self.source,
                &self.route,
                &Usage::default(),
                reason,
            );
            (Usage::default(), None, reason)
        });
        // If the actor was aborted while waiting for this projection, the
        // exact already-settled cost is not billed again. The existing held
        // scheduler retires only worker metadata under the same source id.
        self.job.authority.accounting_projection().recover_settled(
            &self.job.scheduler,
            self.source.clone(),
            self.route.clone(),
            usage,
            priced,
            reason,
        );
    }
}

#[derive(Debug)]
pub(super) struct UnsettledChildCancellation(pub(super) String);
impl std::fmt::Display for UnsettledChildCancellation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}; approval receipts may remain pending", self.0)
    }
}
impl std::error::Error for UnsettledChildCancellation {}

struct ChildActorGuard {
    handle: crate::core::engine::EngineHandle,
    actor: tokio::task::AbortHandle,
    joined: bool,
}
impl Drop for ChildActorGuard {
    fn drop(&mut self) {
        if self.joined {
            return;
        }
        self.handle.cancel();
        self.actor.abort();
        // Emergency abandonment cannot promise a durable decision. Preserve
        // unmatched Asked receipts for the existing protected recovery reader;
        // routine Stop stays in drive_child_actor through the owned join.
    }
}

/// One existing input's pending-count receipt. The actual Core outcome owns
/// commit/drop; abandoning the transport also retires its UI pending counter.
#[cfg(test)]
pub(crate) fn send_test_child_input(
    tx: &mpsc::UnboundedSender<SubAgentInput>,
    text: &str,
    interrupt: bool,
    pending: Arc<std::sync::atomic::AtomicUsize>,
) {
    tx.send(SubAgentInput {
        text: text.into(),
        interrupt,
        pending: Some(pending),
    })
    .unwrap();
}

struct InputSettlement(SubAgentInput);
impl Drop for InputSettlement {
    fn drop(&mut self) {
        self.0.mark_taken();
    }
}

/// One transport adapter into the same Core actor and its run_turn pipeline.
/// It owns no provider, tool planning, approval decision, or request history.
#[allow(clippy::too_many_arguments)]
pub(super) async fn run_child_agent(
    runtime: &SubAgentRuntime,
    agent_id: String,
    agent_type: FleetRole,
    prompt: String,
    assignment: SubAgentAssignment,
    allowed_tools: Option<Vec<String>>,
    fork_context: bool,
    started_at: Instant,
    max_steps: u32,
    parking: Option<Arc<std::sync::atomic::AtomicBool>>,
    input_rx: mpsc::UnboundedReceiver<SubAgentInput>,
) -> Result<SubAgentResult> {
    let (runtime, attachment) = if runtime.cancel_token.is_cancelled() {
        (runtime.clone(), None)
    } else {
        prepare_child_membership(runtime, &assignment, &agent_id).await?
    };
    let authority = ChildAuthority::capture(
        runtime.clone(),
        agent_type.clone(),
        agent_id.clone(),
        assignment
            .role
            .clone()
            .unwrap_or_else(|| agent_type.as_str().to_owned()),
        allowed_tools.clone(),
    );
    let system =
        build_subagent_system_prompt_with_skills(&agent_type, &assignment, &runtime.context);
    let work_max_steps = if max_steps >= 2 {
        max_steps - 1
    } else {
        max_steps
    };
    let prompt = format!(
        "{prompt}\n\n{}",
        child_runtime_budget_context(&runtime, max_steps, work_max_steps)
    );
    let fork = if fork_context {
        match runtime.fork_context.as_ref() {
            Some(fork) => Some(fork.with_resolved_state_block().await),
            None => None,
        }
    } else {
        None
    };
    let mut seed = build_initial_subagent_messages_with_system(
        &prompt,
        &assignment,
        &agent_type,
        &system,
        fork.as_ref(),
    );
    let initial = seed
        .pop()
        .ok_or_else(|| anyhow!("child assignment has no task message"))?;
    let content = initial
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    let job = ChildJob::admitted(
        authority.clone(),
        assignment,
        started_at,
        max_steps,
        fork_context,
        parking,
    )
    .await?;
    if runtime.cancel_token.is_cancelled() {
        // No tool/model is admitted. Preserve the same full seed and the
        // existing Park-vs-Cancel projection, including a step-zero checkpoint.
        seed.push(initial);
        job.project(&seed, 0, None).await?;
        let checkpoint = runtime
            .manager
            .read()
            .await
            .get_result(&agent_id)
            .ok()
            .and_then(|result| result.checkpoint);
        let mut artifact = job.artifact.lock().await;
        return Ok(cancelled_subagent_result(
            &runtime,
            &agent_id,
            &agent_type,
            &job.assignment,
            &seed,
            0,
            max_steps,
            checkpoint.as_ref(),
            job.parking.as_ref(),
            &mut artifact,
            started_at,
            fork_context,
            " before Core child admission",
        )
        .await);
    }
    let api = runtime.api_config.as_deref().ok_or_else(|| anyhow!(
        "child Engine requires its captured provider configuration; no ambient route is admitted"
    ))?;
    let config = crate::core::engine::EngineConfig {
        workspace: runtime.context.workspace.clone(),
        model: runtime.model.clone(),
        max_steps: crate::core::engine::turn_budget::resolve_max_model_steps(Some(work_max_steps)),
        compaction: runtime.compaction.clone(),
        subagent_api_timeout: runtime.step_api_timeout,
        auto_review_policy: runtime.auto_review_policy.as_ref().clone(),
        tools_always_load: allowed_tools.iter().flatten().cloned().collect(),
        locale_tag: runtime.locale_tag.clone(),
        ..Default::default()
    };
    let (mut core, handle) = crate::core::engine::Engine::new_child_admitted(
        config,
        api,
        authority,
        subagent_request_system_prompt(&system),
        attachment,
    )?;
    core.install_child_job(job.clone(), seed)?;
    let spec = core.child_turn_spec(content, allowed_tools)?;
    handle.send(crate::core::ops::Op::SendMessage(spec)).await?;
    drive_child_actor(core, handle, job, input_rx).await
}

/// Project an already-decided Core observation under its captured owner.
/// The existing audit/approval writers retain authority; capacity waits are
/// bounded by the caller's cancellation and captured deadline/tool timeout.
pub(crate) async fn forward_child_gate_observation(
    runtime: &SubAgentRuntime,
    owner: &str,
    mut event: Event,
    cancel: &CancellationToken,
    deadline: Option<tokio::time::Instant>,
) {
    let Event::ToolGateDecision { agent_id, .. } = &mut event else {
        return;
    };
    if agent_id.is_none() {
        *agent_id = Some(owner.to_owned());
    }
    let Some(tx) = runtime.event_tx.as_ref() else {
        return;
    };
    let deadline = deadline
        .into_iter()
        .chain(runtime.context.turn_deadline)
        .chain(Some(tokio::time::Instant::now() + runtime.tool_timeout))
        .min()
        .expect("captured tool timeout bounds observation delivery");
    let permit = tokio::select! {
        biased;
        () = runtime.cancel_token.cancelled() => None,
        () = cancel.cancelled() => None,
        result = tokio::time::timeout_at(deadline, tx.reserve()) => result.ok().and_then(Result::ok),
    };
    if let Some(permit) = permit {
        permit.send(event);
    } else {
        // Available capacity can still carry the final observation after
        // cancellation. A full/closed host cannot keep the actor alive.
        let _ = tx.try_send(event);
    }
}

/// Service the existing Core event/approval/input queues until its actor
/// settles. Shared by the real launch and its exact Core transport tests.
pub(crate) async fn drive_child_actor(
    core: crate::core::engine::Engine,
    handle: crate::core::engine::EngineHandle,
    job: Arc<ChildJob>,
    mut input_rx: mpsc::UnboundedReceiver<SubAgentInput>,
) -> Result<SubAgentResult> {
    let runtime = job.authority.runtime.clone();
    let agent_id = job.authority.owner_agent_id.clone();
    let mut actor = tokio::spawn(Box::pin(core.run_child()));
    let mut guard = ChildActorGuard {
        handle: handle.clone(),
        actor: actor.abort_handle(),
        joined: false,
    };
    let mut approvals = futures_util::stream::FuturesUnordered::new();
    let mut steers = futures_util::stream::FuturesUnordered::new();
    let mut events = handle.rx_event.write().await;
    let mut input_open = true;
    let mut pending_input: Option<InputSettlement> = None;
    let mut cancellation_deadline = None;
    let mut tool_names: HashMap<String, String> = HashMap::new();
    // The launch permit alone is not a started turn. Project only Core's
    // admitted lifecycle event, once across work, report and route retries.
    let mut started = false;
    let mut observe_start = |event: &Event| {
        if !started && matches!(event, Event::TurnStarted { .. }) {
            started = true;
            if let Some(mailbox) = runtime.mailbox.as_ref() {
                let _ = mailbox.send(MailboxMessage::started(
                    &agent_id,
                    job.authority.agent_type.clone(),
                ));
            }
        }
    };
    let mut result = loop {
        if runtime.cancel_token.is_cancelled() && cancellation_deadline.is_none() {
            handle.cancel();
            cancellation_deadline = Some(tokio::time::Instant::now() + CHILD_STOP_SETTLE_GRACE);
        }
        use futures_util::StreamExt;
        tokio::select! {
            biased;
            () = runtime.cancel_token.cancelled(), if !handle.is_cancelled() => handle.cancel(),
            joined = &mut actor => {
                guard.joined = true;
                break joined.map_err(|e| anyhow!(UnsettledChildCancellation(format!("child Core actor failed: {e}"))))?;
            },
            () = async {
                match cancellation_deadline {
                    Some(deadline) => tokio::time::sleep_until(deadline).await,
                    None => std::future::pending::<()>().await,
                }
            } => {
                actor.abort();
                let _ = (&mut actor).await;
                guard.joined = true;
                break Err(anyhow!(UnsettledChildCancellation("child cancellation settlement deadline elapsed".into())));
            },
            input = input_rx.recv(), if input_open && pending_input.is_none() => match input {
                Some(input) => pending_input = Some(InputSettlement(input)),
                None => input_open = false,
            },
            reservation = handle.reserve_steer(), if pending_input.is_some() => {
                let input = pending_input.take().expect("selected pending input");
                match reservation {
                    Ok(permit) => {
                        let outcome = if input.0.interrupt { permit.send_replacing_with_outcome(input.0.text.clone()) }
                            else { permit.send_with_outcome(input.0.text.clone()) };
                        steers.push(async move { let result = outcome.await; (input, result) });
                    }
                    Err(_) => drop(input), // Core closed before admission: explicitly dropped.
                }
            },
            Some((input, _outcome)) = steers.next(), if !steers.is_empty() => {
                // Core's commit/drop outcome, not transport admission, retires it.
                drop(input);
            },
            Some((id, decision)) = approvals.next(), if !approvals.is_empty() => {
                // Manager removed the slot before this exact Core inbox delivery.
                if runtime.cancel_token.is_cancelled() || handle.is_cancelled() {
                    // Core observes the exact cancelled TurnControl before any
                    // queued Allow; do not stall its join on another inbox send.
                    handle.cancel();
                } else {
                    match decision {
                        Ok(ChildApprovalOutcome::Approved { by }) => handle.approve_tool_call_by(id, by).await?,
                        Ok(ChildApprovalOutcome::Denied { by }) => handle.deny_tool_call_by(id, by).await?,
                        _ => handle.deny_tool_call_unavailable(id).await?,
                    }
                }
            },
            event = events.recv() => {
                let Some(event) = event else { break Err(anyhow!("child Core event stream closed before join")); };
                observe_start(&event);
                match &event {
                    Event::ApprovalRequired { id, tool_name, description, .. } => {
                        let (_, answer) = runtime.manager.write().await.register_child_approval(&agent_id, id, tool_name, description)?;
                        let key = id.clone(); approvals.push(async move { (key, answer.await) });
                        record_agent_progress(&runtime, &agent_id, AgentProgressEventMeta::new(AgentWorkerStatus::WaitingForUser)
                            .with_tool(tool_name.clone()).with_approval_id(id.clone()), description.clone());
                        if let Some(tx) = runtime.event_tx.as_ref().filter(|_| runtime.parent_can_prompt) {
                            let admitted_cancel = handle.captured_turn_cancel();
                            let sent = tokio::select! {
                                biased;
                                () = runtime.cancel_token.cancelled() => false,
                                () = admitted_cancel.cancelled() => false,
                                result = tx.send(event.clone()) => result.is_ok(),
                            };
                            if !sent {
                                if runtime.cancel_token.is_cancelled() || handle.is_cancelled() {
                                    handle.cancel();
                                } else {
                                    runtime.manager.write().await.cancel_child_approval(id);
                                    handle.deny_tool_call_unavailable(id).await?;
                                }
                            }
                        } else if runtime.cancel_token.is_cancelled() || handle.is_cancelled() {
                            handle.cancel();
                        } else {
                            runtime.manager.write().await.cancel_child_approval(id);
                            handle.deny_tool_call_unavailable(id).await?;
                        }
                    }
                    Event::ApprovalWithdrawn { id } => {
                        runtime.manager.write().await.cancel_child_approval(id);
                        if let Some(tx) = runtime.event_tx.as_ref() {
                            tokio::select! {
                                biased;
                                () = runtime.cancel_token.cancelled() => { let _ = tx.try_send(event.clone()); }
                                result = tx.send(event.clone()) => { let _ = result; }
                            }
                        }
                    }
                    Event::ToolGateDecision { .. } => {
                        forward_child_gate_observation(
                            &runtime,
                            &agent_id,
                            event,
                            &handle.captured_turn_cancel(),
                            job.hard_deadline.map(Into::into),
                        ).await;
                    }
                    Event::ToolCallStarted { id, name, .. } => {
                        tool_names.insert(id.clone(), name.clone());
                    }
                    Event::ToolExecutionStarted { id } => {
                        if let Some(mailbox) = runtime.mailbox.as_ref() {
                            let tool_name = tool_names.get(id).cloned().unwrap_or_else(|| "tool".to_string());
                            let _ = mailbox.send(MailboxMessage::ToolCallStarted { agent_id: agent_id.clone(), tool_name, step: job.steps() });
                        }
                    }
                    Event::ToolCallComplete { id, name, result, .. } => {
                        let label = tool_names.remove(id).unwrap_or_else(|| name.clone());
                        record_agent_progress(&runtime, &agent_id, AgentProgressEventMeta::new(AgentWorkerStatus::RunningTool).with_tool(name.clone()),
                            format!("{label} {}", if result.is_ok() { "done" } else { "failed" }));
                    }
                    Event::Status { message, .. } => record_agent_progress(&runtime, &agent_id,
                        AgentProgressEventMeta::new(AgentWorkerStatus::Running), message.clone()),
                    _ => {} // Session is projected by Core; aggregate usage is never rebilled here.
                }
            },
        }
    };
    if !guard.joined {
        handle.cancel();
        result = match tokio::time::timeout(CHILD_STOP_SETTLE_GRACE, &mut actor).await {
            Ok(joined) => joined.map_err(|error| {
                anyhow!(UnsettledChildCancellation(format!(
                    "child Core actor failed: {error}"
                )))
            })?,
            Err(_) => {
                actor.abort();
                let _ = (&mut actor).await;
                Err(anyhow!(UnsettledChildCancellation(
                    "child cancellation settlement deadline elapsed".into()
                )))
            }
        };
        guard.joined = true;
    }
    // The join can win over events already queued by Core. Retain its start
    // and final gate observations; this projection never asks or decides a call.
    while let Ok(event) = events.try_recv() {
        observe_start(&event);
        if let Event::ApprovalWithdrawn { id } = &event {
            runtime.manager.write().await.cancel_child_approval(id);
            if let Some(tx) = &runtime.event_tx {
                let _ = tx.try_send(event);
            }
        } else if matches!(event, Event::ToolGateDecision { .. }) {
            forward_child_gate_observation(
                &runtime,
                &agent_id,
                event,
                &handle.captured_turn_cancel(),
                job.hard_deadline.map(Into::into),
            )
            .await;
        }
    }
    // Joining drops the Core receiver and every unsettled steer. Retire those
    // exact outcomes even when the join branch won before the outcome branch.
    use futures_util::StreamExt;
    while let Some((input, _outcome)) = steers.next().await {
        drop(input);
    }
    drop(pending_input);
    while let Ok(input) = input_rx.try_recv() {
        input.mark_taken();
    }
    let pending = runtime
        .manager
        .read()
        .await
        .pending_requests_for_agent(&agent_id);
    for pending in pending {
        runtime
            .manager
            .write()
            .await
            .cancel_child_approval(&pending.approval_id);
        if let Some(tx) = runtime.event_tx.as_ref() {
            let event = Event::ApprovalWithdrawn {
                id: pending.approval_id,
            };
            if runtime.cancel_token.is_cancelled() {
                let _ = tx.try_send(event);
            } else if let Some(deadline) = job.hard_deadline {
                let _ = tokio::time::timeout_at(deadline.into(), tx.send(event)).await;
            } else {
                let _ = tx.send(event).await;
            }
        }
    }
    result
}

async fn prepare_child_membership(
    runtime: &SubAgentRuntime,
    assignment: &SubAgentAssignment,
    agent_id: &str,
) -> Result<(
    SubAgentRuntime,
    Option<crate::extension_host::HostAttachment>,
)> {
    let mut scoped_runtime = runtime.clone();
    let extension_host = if crate::plugins::activation::extension_host_policy_enabled() {
        if let Some(plugins) = runtime.context.plugin_registry.as_ref() {
            let selected = if let Some(preset) = assignment.native_preset.as_ref() {
                let plugins = Arc::clone(plugins);
                let preset = preset.clone();
                let policy = crate::plugins::activation::extension_host_policy_enabled();
                #[cfg(test)]
                let env_scope = crate::test_support::env_scope_ticket();
                tokio::task::spawn_blocking(move || {
                    #[cfg(test)]
                    let _env_scope = crate::test_support::join_env_scope(env_scope);
                    let _policy = crate::plugins::activation::PolicyScope::propagate(policy);
                    plugins.with_native_preset(preset)
                })
                .await
                .map_err(|error| anyhow!("Native preset validation worker failed: {error}"))?
                .map_err(anyhow::Error::msg)?
            } else {
                plugins.as_ref().clone()
            };
            let attachment = crate::extension_host::manager().attach(Arc::new(selected));
            attachment.set_identity(
                runtime
                    .context
                    .execution
                    .session_objects
                    .as_ref()
                    .map(|session| session.session_id.clone()),
                Some(agent_id.to_owned()),
            );
            attachment.reconcile().await.map_err(anyhow::Error::msg)?;
            scoped_runtime.context = scoped_runtime
                .context
                .clone()
                .with_plugin_registry(attachment.plugin_view());
            if let Some(parent_pool) = runtime.mcp_pool.as_ref() {
                let mut pool = parent_pool
                    .lock()
                    .await
                    .fork_for_plugins(attachment.plugin_view())?;
                // Discover this caller's selected catalog through the existing
                // Core connection factory before the child registry snapshots it.
                // Optional connection failures retain Core's lazy retry behavior.
                for (name, error) in pool.connect_all().await {
                    tracing::warn!("child MCP server {name} unavailable: {error}");
                }
                scoped_runtime.mcp_pool = Some(Arc::new(tokio::sync::Mutex::new(pool)));
            }
            Some(attachment)
        } else if assignment.native_preset.is_some() {
            return Err(anyhow!(
                "Native composition requires the caller's reviewed plugin inventory"
            ));
        } else {
            None
        }
    } else if assignment.native_preset.is_some() {
        return Err(anyhow!(
            "Native composition requires Experimental extension_host"
        ));
    } else {
        None
    };

    Ok((scoped_runtime, extension_host))
}

/// Counters for one logical Engine model step. They do not dispatch or own
/// a turn; the existing outer run_turn loop consumes this policy decision.
#[derive(Default)]
pub(crate) struct ChildRequestRetries {
    transient: u32,
    timeouts: u32,
}
pub(crate) enum ChildRequestRecovery {
    Retry {
        delay: Duration,
        note: String,
    },
    Interrupted {
        checkpoint_reason: &'static str,
        message: String,
    },
}
impl ChildRequestRetries {
    pub(crate) fn decide(
        &mut self,
        runtime: &SubAgentRuntime,
        error: &anyhow::Error,
    ) -> Option<ChildRequestRecovery> {
        if matches!(error.downcast_ref::<LlmError>(), Some(LlmError::Timeout(_))) {
            if self.timeouts >= SUBAGENT_API_TIMEOUT_MAX_RETRIES {
                return Some(ChildRequestRecovery::Interrupted {
                    checkpoint_reason: "api_timeout",
                    message: format!(
                        "API call timed out after {}ms on {} API attempt(s); checkpoint preserved for continuation",
                        runtime.step_api_timeout.as_millis(),
                        self.timeouts.saturating_add(1)
                    ),
                });
            }
            self.timeouts = self.timeouts.saturating_add(1);
            let delay = subagent_api_timeout_retry_delay(
                self.timeouts,
                runtime.api_timeout_retry_base_backoff,
            );
            return Some(ChildRequestRecovery::Retry {
                delay,
                note: format!(
                    "API call timed out after {}ms; retrying API request {}/{} in {}ms",
                    runtime.step_api_timeout.as_millis(),
                    self.timeouts,
                    SUBAGENT_API_TIMEOUT_MAX_RETRIES,
                    delay.as_millis()
                ),
            });
        }
        let retryable =
            retryable_subagent_provider_failure(error, self.transient.saturating_add(1))?;
        if self.transient >= SUBAGENT_TRANSIENT_PROVIDER_MAX_RETRIES {
            return Some(ChildRequestRecovery::Interrupted {
                checkpoint_reason: retryable.checkpoint_reason,
                message: format!(
                    "{} after {} API attempt(s): {error:#}; checkpoint preserved for continuation",
                    retryable.label,
                    self.transient.saturating_add(1)
                ),
            });
        }
        self.transient = self.transient.saturating_add(1);
        Some(ChildRequestRecovery::Retry {
            delay: retryable.delay,
            note: format!(
                "{}; retrying API request {}/{} in {}ms ({error:#})",
                retryable.label,
                self.transient,
                SUBAGENT_TRANSIENT_PROVIDER_MAX_RETRIES,
                retryable.delay.as_millis()
            ),
        })
    }
}

/// Reuse the approved first-request route policy. The caller installs it in
/// the existing Engine; this helper has no provider, planner or retry loop.
pub(crate) fn approved_first_request_replacement(
    job: &ChildJob,
    current: &SubAgentRuntime,
    replacements_tried: &mut usize,
    pin_fallback_used: &mut bool,
    error: &anyhow::Error,
) -> Result<Option<(SubAgentRuntime, SpawnRouteSource, String)>> {
    if !job.can_replace_first_request() {
        return Ok(None);
    }
    let detail: String = current
        .client
        .redact_model_bound_text(&format!("{error}"))
        .chars()
        .take(160)
        .collect();
    if !*pin_fallback_used
        && let Some(origin) = current.route_origin.as_deref()
        && let Some(parent) = origin.parent.as_ref()
        && let Some(why) = pin_refusal_reason(error)
    {
        *pin_fallback_used = true;
        let note: String = format!(
            "{}, which failed authorization ({why}: {detail}); ran on {} instead",
            origin.source, parent.label
        )
        .chars()
        .take(480)
        .collect();
        let mut next = current.clone();
        parent.install(&mut next);
        next.route_origin = Some(Arc::new(spawn_route_origin(
            SpawnRouteSource::SessionFallback,
            current.worker_profile.role.as_str(),
            None,
            parent.label.clone(),
            Some(&note),
        )));
        return Ok(Some((next, SpawnRouteSource::SessionFallback, note)));
    }
    let Some(why) = route_replacement_reason(error) else {
        return Ok(None);
    };
    let original = &job.authority.runtime;
    let mut skipped = Vec::new();
    while let Some(route) = original.route_replacements.get(*replacements_tried) {
        *replacements_tried += 1;
        let to = format!(
            "{}/{}",
            route.provider.as_deref().unwrap_or_default(),
            route.model
        );
        match replacement_route_runtime(original, route) {
            Ok(mut next) => {
                let from = format!(
                    "{}/{}",
                    current.client.api_provider().as_str(),
                    current.model
                );
                let mut note = format!(
                    "{from} refused the first request before any work ({why}: {detail}); moved to approved replacement {to} (attempt {} of {})",
                    *replacements_tried,
                    original.route_replacements.len()
                );
                if !skipped.is_empty() {
                    note.push_str("; skipped ");
                    note.push_str(&skipped.join("; "));
                }
                let note: String = note.chars().take(480).collect();
                next.route_origin = Some(Arc::new(spawn_route_origin(
                    SpawnRouteSource::RoleReplacement,
                    original.worker_profile.role.as_str(),
                    None,
                    runtime_route_label(&next),
                    Some(&note),
                )));
                return Ok(Some((next, SpawnRouteSource::RoleReplacement, note)));
            }
            Err(unavailable) => skipped.push(format!(
                "{to} ({})",
                unavailable.chars().take(120).collect::<String>()
            )),
        }
    }
    if skipped.is_empty() {
        Ok(None)
    } else {
        Err(anyhow!(
            "no approved replacement route could take the task: {}",
            skipped.join("; ")
        ))
    }
}

#[cfg(test)]
mod owner_origin_tests {
    use super::*;

    #[tokio::test]
    async fn canonical_child_unbounded_claim_checks_exact_held_origin_before_peer_projection() {
        let _environment = crate::test_support::lock_test_env();
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().canonicalize().unwrap();
        let original = root.join("original");
        let selected = root.join("selected");
        fs::create_dir_all(original.join("src")).unwrap();
        fs::create_dir_all(selected.join("src")).unwrap();
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", root.join("home"));
        let manager = new_shared_subagent_manager(original.clone(), 2);
        {
            let mut held = manager.write().await;
            for lexical in [&original, &selected] {
                let (canonical, file) =
                    crate::runtime_api::open_workspace_directory(lexical).unwrap();
                held.admit_coordination_workspace(lexical.clone(), canonical, Arc::new(file))
                    .unwrap();
            }
            let spec = super::super::owner_scoped_coordination_tests::writer(
                "selected-writer",
                &selected,
                "src",
            );
            held.register_worker_with_coordination(spec).unwrap();
        }
        let mut runtime = super::super::tests::stub_runtime();
        runtime.manager = manager.clone();
        runtime.context = ToolContext::new(selected.clone());
        runtime.worker_profile = WorkerRuntimeProfile::for_role(FleetRole::Builder);
        runtime.allow_shell = true;
        let authority = ChildAuthority::capture(
            runtime,
            FleetRole::Builder,
            "selected-writer".into(),
            "writer".into(),
            None,
        );
        let mut registry = ToolRegistry::new(ToolContext::new(selected.clone()));
        registry.register(Arc::new(crate::tools::shell::BashTool::new("Bash")));
        let input = serde_json::json!({"command":"python3 -c 'print(1)'"});
        // No peer exists: ordinary work is still admitted under its held root.
        assert!(
            authority
                .validate_claim(&registry, "bash", &input)
                .await
                .unwrap()
                .is_empty()
        );
        {
            let mut held = manager.write().await;
            let foreign = held.admitted_coordination_roots[&original].receipt.clone();
            held.worker_origins
                .get_mut("selected-writer")
                .unwrap()
                .scope = Some(foreign);
        }
        let refused = authority
            .validate_claim(&registry, "bash", &input)
            .await
            .unwrap_err();
        assert!(
            refused.to_string().contains("owner re-admission"),
            "{refused}"
        );
        assert!(
            manager
                .read()
                .await
                .live_peer_shared_write_claim_owners("selected-writer")
                .is_empty()
        );
        assert!(!selected.join("effect").exists());
    }
}

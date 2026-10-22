//! Engine-side authority for `/preview-request` (#1004, #3928).
//!
//! The preview lives here — not in the command layer — because only the
//! engine can rebuild the *exact* next-turn state: the tool catalog under the
//! live mode, gates, permission posture and connected MCP tools; the system
//! prompt for the route the next turn would use; the hypothetical next user
//! message in its production form; and the request the turn loop would hand
//! to `create_message_stream`.
//!
//! Four rules this module exists to enforce:
//!
//! - **Never `session.last_tool_catalog`.** That value is one turn stale and
//!   stores the pre-activation catalog, so it cannot describe what the *next*
//!   request would send. The catalog is rebuilt through
//!   [`Engine::build_turn_tool_registry_and_catalog`], which returns the same
//!   typed policy a real turn consumes.
//! - **Never invent a route.** For fixed routes, the host resolves the next
//!   turn through the same shared planner production dispatch uses. Auto would
//!   require a model-classifier call, so the human preview stops before the
//!   planner and emits a typed unavailable state. No route, endpoint, wire
//!   model, billing, tool budget, or body hash is recycled from the installed
//!   route.
//! - **Never resolve by side effect.** The catalog build runs with
//!   [`SubAgentWiring::Inert`] and [`McpAccess::PassiveSnapshot`]: no fork
//!   snapshot, no spawned drainer, no MCP pool creation, no `connect_all`, no
//!   status events. When the connected MCP state is not already exactly what
//!   a turn would use, the tool section is reported unavailable rather than
//!   made exact by connecting — **and so is the body**, because a body built
//!   from a tool surface missing its MCP contribution is a body no turn would
//!   send.
//! - **Never install anything, not even briefly.** The planned route is
//!   projected into a throw-away client; `self.api_provider`,
//!   `self.session.model`, `self.session.system_prompt`, and the MCP pool are
//!   all left untouched. Everything a turn would *install before* building its
//!   request — the command-scoped tool gate, the effective mode and approval
//!   posture, the policy-narrowing event, the observed working set — is passed
//!   as a value or snapshotted onto a clone. There is no write-then-restore
//!   anywhere in this module: a restore is not atomic across an `.await`, and
//!   it does not survive a cancellation or a panic.
//! - **Never claim exactness the runtime would break.** Mutable
//!   `message_submit` hooks, background-shell completions, running or
//!   terminal-undelivered sub-agent completions,
//!   pending LSP diagnostics, auto-compaction, and context-overflow recovery
//!   all rewrite the request between submit and the wire. An inspection may
//!   neither run them nor consume them, so when any of them apply the affected
//!   sections are typed unavailable instead of published.
//!
//! Scope: this describes the primary agent turn (`create_message_stream`).
//! Auxiliary provider calls are out of scope; see `docs/PREVIEW_REQUEST.md`.
//!
//! The `dryrun` concept — preview the next request from the real
//! request-building seam rather than a hand-rolled summary — is harvested
//! from PR #1099 by TaoMu (GTC2080).

use super::*;

use crate::client::PreparedOutboundRequest;
use crate::compaction::should_compact;
use crate::request_manifest::{
    Availability, BasePromptProvenance, BillingFacts, ManifestDraft, PreparedBodyInputs,
    PromptProvenance, ReasoningResolution, RequestManifest, RouteFacts, SessionFacts,
    SystemPromptAssembly, ToolSurfaceFacts, UnavailableReason,
};
use crate::route_runtime::ResolvedRuntimeRoute;
use crate::safe_label::SafeLabel;
use codewhale_core::request::{PrimaryTurnRequest, prepare_primary_turn_request};

/// Everything the host must supply for the engine to describe the next
/// request. These are the same posture fields a `SendMessage` would carry, so
/// the preview describes the turn the user is actually about to run.
#[derive(Debug)]
pub struct PreviewRequestInputs {
    pub mode: AppMode,
    pub allow_shell: bool,
    pub trust_mode: bool,
    pub auto_approve: bool,
    pub approval_mode: ApprovalMode,
    pub allowed_tools: Option<Vec<String>>,
    pub dynamic_tools: Vec<DynamicToolSpec>,
    pub provenance: UserInputProvenance,
    /// The model selector the user chose: `auto` when auto model routing is
    /// on. Never the concrete model an unresolved auto route might pick.
    pub requested_model: String,
    /// Reasoning tier the user has selected (`auto`, `high`, `off`, …).
    pub requested_reasoning: String,
    pub auto_model: bool,
    /// Whether the *caller* supplied a hypothetical next prompt.
    ///
    /// Deliberately independent of [`Self::next_turn`]: when planning that
    /// prompt fails, the manifest must still say a prompt was supplied.
    /// Deriving the flag from `next_turn.is_some()` told the user to "pass
    /// `--prompt`" when they just had.
    pub hypothetical_prompt_supplied: bool,
    /// The exact next turn, resolved by the host's shared route planner.
    /// `None` means no exact next turn exists to describe.
    pub next_turn: Option<Box<PreviewNextTurn>>,
    /// Why `next_turn` is absent. Ignored when `next_turn` is present.
    pub unresolved: PreviewUnresolved,
}

/// One hypothetical next turn, planned by the production route planner.
#[derive(Debug)]
pub struct PreviewNextTurn {
    /// The model-facing text of the hypothetical user message, already
    /// through the host's file-mention/skill resolution — the same string a
    /// real `SendMessage` would carry. Never stored in the session and never
    /// sent to a provider.
    pub content: String,
    /// The route the planner resolved for this turn.
    pub route: Box<ResolvedRuntimeRoute>,
    /// Immutable prompt facts captured from the same host state as the
    /// matching production submit.
    pub prompt_context: NextTurnPromptContext,
    /// Normalized reasoning-effort api value from the planner, exactly as it
    /// would be sent.
    pub reasoning_effort: Option<String>,
    /// True when the user selected auto reasoning and the planner picked that
    /// tier.
    pub reasoning_effort_auto: bool,
    /// How the auto router chose this route, when auto routing ran.
    pub auto_route_source: Option<String>,
    /// Typed selection provenance captured by the shared production planner.
    pub routing_source: crate::turn_route_plan::TurnRoutingSource,
    /// The compaction policy the planner resolved for this route. A real turn
    /// installs it before the turn loop decides whether to auto-compact, so
    /// the preview evaluates that decision against the same policy.
    pub compaction: crate::compaction::CompactionConfig,
}

/// Why no exact next turn was planned.
#[derive(Debug, Clone)]
pub enum PreviewUnresolved {
    /// Auto model routing is on and no hypothetical prompt was supplied.
    AutoRouteNeedsPrompt,
    /// Auto model routing needs a classifier provider call. A preview is
    /// strictly offline, so it stops before invoking the shared route planner.
    AutoRouteClassificationNotExecuted,
    /// No hypothetical prompt was supplied, so there is no next-turn body.
    NoPrompt,
    /// The shared planner ran and failed. Carries raw host text; it crosses
    /// the safe-label boundary before it reaches any surface.
    PlanFailed(String),
    /// Mutable `message_submit` hooks are configured. A real submit runs them
    /// before file mentions, skill wrapping, route planning, and the tool
    /// policy see the text, and they may replace or block it outright. An
    /// inspection must not execute a hook, so nothing downstream of the text
    /// — route, tools, or body — can be claimed exact.
    MessageSubmitHooksConfigured,
    /// Resolving the prompt into model-facing content failed exactly as a real
    /// submit would have failed. Carries raw host text.
    PromptResolutionFailed(String),
}

impl PreviewUnresolved {
    fn as_availability<T>(&self) -> Availability<T> {
        match self {
            Self::AutoRouteNeedsPrompt => {
                Availability::unavailable(UnavailableReason::AutoRouteUnresolvedUntilNextPrompt)
            }
            Self::AutoRouteClassificationNotExecuted => {
                Availability::unavailable(UnavailableReason::AutoRouteClassificationNotExecuted)
            }
            Self::NoPrompt => {
                Availability::unavailable(UnavailableReason::NoHypotheticalPromptSupplied)
            }
            Self::PlanFailed(error) => {
                Availability::unavailable_with(UnavailableReason::RoutePlanFailed, error.clone())
            }
            Self::MessageSubmitHooksConfigured => {
                Availability::unavailable(UnavailableReason::MessageSubmitHooksNotExecuted)
            }
            Self::PromptResolutionFailed(error) => Availability::unavailable_with(
                UnavailableReason::PromptResolutionFailed,
                error.clone(),
            ),
        }
    }
}

impl Engine {
    /// Describe the request the next turn would send, without sending it.
    pub(super) async fn build_request_manifest(
        &mut self,
        inputs: PreviewRequestInputs,
    ) -> RequestManifest {
        let session = self.preview_session_facts(&inputs);

        // Mirror terminal continuation gates before request construction.
        // Token budgets are telemetry-only unless `[goal]
        // enforce_token_budget` is set; only then does an exhausted budget
        // make the next goal request unavailable, as the live gates stop.
        let goal_budget_exhausted = match self.config.goal_state.lock() {
            Ok(state) => {
                let snapshot = state.snapshot();
                Ok(snapshot.is_active()
                    && crate::goal_loop::token_budget_exhausted(
                        crate::goal_loop::GoalProgress {
                            tokens_used: snapshot.tokens_used,
                            time_used_seconds: snapshot.time_used_seconds,
                            continuations: snapshot.continuation_count,
                        },
                        crate::goal_loop::GoalBudget {
                            token_budget: snapshot.token_budget.map(u64::from),
                            time_budget_seconds: None,
                            enforce_token_budget: self.config.goal_enforce_token_budget,
                            max_continuations: self.config.goal_max_continuations,
                        },
                    ))
            }
            Err(err) => {
                tracing::warn!("goal state lock poisoned while previewing request: {err}");
                Err(())
            }
        };
        let unavailable_reason = match goal_budget_exhausted {
            Ok(true) => Some(UnavailableReason::GoalTokenBudgetExhausted),
            Ok(false) => None,
            Err(()) => Some(UnavailableReason::GoalStateNotSnapshottable),
        };
        if let Some(reason) = unavailable_reason {
            return RequestManifest::build(ManifestDraft {
                session,
                route: Availability::unavailable(reason),
                tools: Availability::unavailable(reason),
                body: Availability::unavailable(reason),
            });
        }

        let Some(next_turn) = inputs.next_turn else {
            let unresolved = inputs.unresolved;
            return RequestManifest::build(ManifestDraft {
                session,
                route: unresolved.as_availability(),
                tools: unresolved.as_availability(),
                body: unresolved.as_availability(),
            });
        };

        let PreviewNextTurn {
            content: hypothetical_content,
            route: planned_route,
            prompt_context: planned_prompt_context,
            reasoning_effort,
            reasoning_effort_auto,
            auto_route_source,
            routing_source,
            compaction: planned_compaction,
        } = *next_turn;

        // Project the planned route into a throw-away client. `validate`
        // reuses the host's preflighted client when there is one and never
        // touches engine state — unlike `install_resolved_runtime_route`,
        // which is what a real turn calls.
        let route = match (*planned_route).validate() {
            Ok(route) => route,
            Err(error) => {
                let unavailable = PreviewUnresolved::PlanFailed(error);
                return RequestManifest::build(ManifestDraft {
                    session,
                    route: unavailable.as_availability(),
                    tools: unavailable.as_availability(),
                    body: unavailable.as_availability(),
                });
            }
        };

        let provider = route.identity.provider;
        let model = route.model.clone();
        let limits = crate::route_budget::known_route_limits(route.candidate.limits());
        let base_url = route.candidate.endpoint().base_url.clone();
        let route_context = TurnRouteContext {
            provider,
            model: model.clone(),
            capabilities: route.candidate.capabilities(),
            limits,
            client: Some(route.client.clone()),
            api_config: route.config.clone(),
            locale_tag: self.config.locale_tag.clone(),
            role_models: self.subagent_role_models(),
            auto_model: inputs.auto_model,
            reasoning_effort: reasoning_effort.clone(),
            reasoning_effort_auto,
        };

        // Same policy derivation as `handle_send_message`, so the catalog is
        // filtered under the posture the next turn would actually use.
        let input_policy = effective_input_policy(
            inputs.provenance,
            inputs.mode,
            &hypothetical_content,
            inputs.allow_shell,
            inputs.trust_mode,
            inputs.auto_approve,
            inputs.approval_mode,
        );
        let prompt_context = NextTurnPromptContext {
            mode: input_policy.mode,
            ..planned_prompt_context
        };

        // The command-scoped allow gate is *passed*, never installed. The
        // earlier shape wrote `self.config.allowed_tools`, awaited the whole
        // catalog build, and wrote it back: for the duration of that await the
        // engine carried a gate belonging to a turn that was never going to
        // run, and a cancellation or panic in between would have left it
        // installed for good.
        let build = self
            .build_turn_tool_registry_and_catalog(
                &input_policy,
                &inputs.dynamic_tools,
                inputs.allowed_tools.clone(),
                SubAgentWiring::Inert,
                McpAccess::PassiveSnapshot,
                route_context.clone(),
                "",
                prompt_context.goal_objective.as_deref(),
            )
            .await;

        // The build owns the exact same initial subset dispatch consumes.
        let surface = &build.surface;
        let active_tools = surface.active.clone().unwrap_or_default();
        let active_catalog_sha256 = active_tool_catalog_sha256(&active_tools);

        let tool_choice = surface.active.as_ref().map(|_| {
            if surface.strict_tool_mode {
                json!("required")
            } else {
                json!({ "type": "auto" })
            }
        });

        // The tool surface is only publishable when the MCP contribution is
        // exactly known. Anything else would be "the tools of some other
        // turn", which is the failure mode this command exists to avoid.
        let tools = match build.mcp.server_count() {
            Some(mcp_server_count) => Availability::Exact(ToolSurfaceFacts {
                catalog_tool_count: surface.catalog.len(),
                deferred_tool_count: surface
                    .catalog
                    .iter()
                    .filter(|tool| tool.defer_loading.unwrap_or(false))
                    .count(),
                active_tool_count: active_tools.len(),
                active_tool_catalog_sha256: active_catalog_sha256,
                tool_surface_budget: format!(
                    "{:?}",
                    route_context.capability_profile().tool_surface_budget
                ),
                standard_and_full_surfaces_collapsed: standard_and_full_collapse(
                    &surface.catalog,
                    &self.config.tools_always_load,
                ),
                mcp_server_count,
                mcp_tool_count: active_tools
                    .iter()
                    .filter(|tool| build.mcp_tool_names.contains(&tool.name))
                    .count(),
            }),
            None => match &build.mcp {
                McpToolState::Unavailable { reason } => Availability::unavailable_with(
                    UnavailableReason::McpStateNotSnapshottable,
                    reason.label(),
                ),
                McpToolState::Disabled | McpToolState::Live { .. } => {
                    Availability::unavailable(UnavailableReason::McpStateNotSnapshottable)
                }
            },
        };

        // The system prompt a turn would send is composed for *its* route, so
        // an auto-routed preview must not reuse the installed model's prompt.
        // A session-level override wins here exactly as it does in
        // `refresh_system_prompt`.
        // The header is pinned for the session: with unchanged explicit
        // inputs a real turn reuses the pinned bytes (workspace drift arrives
        // as a `<context_update>` message instead), so preview mirrors that.
        let system_prompt = if self.session.system_prompt_override
            || self.session.pinned_prompt_context.as_ref() == Some(&prompt_context)
        {
            self.session.system_prompt.clone()
        } else {
            self.compose_stable_system_prompt(&prompt_context)
        };

        // The hypothetical user message goes through the same constructor
        // production uses — turn metadata, route stamp, and provenance — so
        // the body being hashed is the body a real turn would build. It is
        // appended to a *clone* of the history and discarded: the session
        // never sees it.
        //
        // A real submit calls `working_set.observe_user_message` before it
        // writes `<turn_meta>`, so the block reflects files the new message
        // mentions. The preview observes the message on a **clone** of the
        // working set and builds the block from that snapshot: same bytes, no
        // session write. Nothing here restores state, because nothing here
        // changes any.
        let mut previewed_working_set = self.session.working_set.clone();
        previewed_working_set.observe_user_message(&hypothetical_content, &self.session.workspace);
        // #5187: the git-snapshot line is emitted on change only, tracked in a
        // session cache. Previewing a turn must not advance that cache — the
        // model never saw the previewed block — so the cache is saved and
        // restored around the hypothetical build, same as the working set.
        let previewed_git_snapshot = self
            .last_turn_meta_git_snapshot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let hypothetical_user_message = self.user_text_message_from_snapshot(
            hypothetical_content.clone(),
            &model,
            inputs.auto_model,
            reasoning_effort.as_deref(),
            reasoning_effort_auto,
            inputs.provenance,
            TurnMetadataSnapshot {
                prompt_context: &prompt_context,
                system_prompt: system_prompt.as_ref(),
                approval_mode: input_policy.approval_mode_for_session(),
                working_set: &previewed_working_set,
                policy_narrowing: input_policy.narrowing.as_ref(),
            },
        );
        *self
            .last_turn_meta_git_snapshot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = previewed_git_snapshot;
        // Classification input for the provenance section: the prompt this
        // request actually carries, not the session's current one.
        let system_prompt_text =
            codewhale_core::prefix_cache::system_prompt_text(system_prompt.as_ref());

        let mut messages = self.messages_with_turn_metadata();
        messages.push(hypothetical_user_message);

        // Transforms the turn loop would apply to this conversation between
        // dispatch and the wire. Detected read-only; nothing pending is
        // consumed, drained, or flushed by looking.
        let mut runtime_transforms = self
            .preview_runtime_transforms(&messages, system_prompt.as_ref(), &planned_compaction)
            .await;

        // The turn loop resolves an `auto` sentinel tier to its declared
        // policy value, *after* the planner normalized it. Skipping that step
        // described a request carrying a literal `auto`, which no route
        // receives.
        let effective_reasoning_effort = super::turn_loop::resolve_auto_effort(
            reasoning_effort.as_deref(),
            provider,
            &base_url,
            &model,
        );

        // Production sends stored history and nothing else — no synthetic
        // To-do block, on any step — so the previewed outbound message list is
        // exactly the message list.
        let outbound_messages = messages.clone();

        // The production overflow gate estimates the logical messages and
        // system prompt, not serialized provider-body bytes. Use that same
        // contract here; the manifest keeps its wire estimate separately as
        // an observability metric.
        let production_input_estimate_tokens =
            crate::compaction::estimate_input_tokens_conservative(
                &messages,
                system_prompt.as_ref(),
            );

        let mut request = prepare_primary_turn_request(PrimaryTurnRequest {
            model: model.clone(),
            messages: outbound_messages,
            max_tokens: effective_max_output_tokens_for_route(provider, &model, limits),
            system: system_prompt,
            tools: surface.active.clone(),
            tool_choice: tool_choice.clone(),
            reasoning_effort: effective_reasoning_effort,
        });
        let (image_input, image_cap) = self.request_image_policy(
            route.identity.key.as_str(),
            &model,
            route_context.capabilities.image_input,
        );
        crate::image_attach::strip_images_when_unsupported(
            &mut request.messages,
            image_input,
            &model,
        );
        if let Some(cap) = image_cap {
            crate::image_attach::cap_request_images(&mut request.messages, cap);
        }

        let prepared = match route.client.prepare_outbound_request(request, true) {
            Ok(prepared) => {
                prepared.with_route_id(route.identity.persisted_id().map(str::to_string))
            }
            Err(error) => {
                let detail = super::turn_loop::preview_request_error_user_message(
                    &self.config.locale_tag,
                    &error,
                );
                // Route identity is read *off the prepared request*, so a
                // preparation failure leaves the endpoint, wire model, and
                // dialect unknown too. The tool surface survives: it was built
                // before the body and does not depend on it.
                return RequestManifest::build(ManifestDraft {
                    session,
                    route: Availability::unavailable_with(
                        UnavailableReason::RequestPreparationFailed,
                        detail.clone(),
                    ),
                    tools,
                    body: Availability::unavailable_with(
                        UnavailableReason::RequestPreparationFailed,
                        detail,
                    ),
                });
            }
        };

        // `include` on a Responses body discloses reasoning output; it does not
        // ask the route to think. Treating any control key as a reasoning
        // request made every Codex turn read as an explicit user selection.
        let reasoning_resolution = if !prepared.reasoning.controls_reasoning() {
            ReasoningResolution::NotApplicable
        } else if reasoning_effort_auto {
            ReasoningResolution::ResolvedFromHypotheticalPrompt
        } else if prepared.reasoning.requested_effort.is_none() {
            ReasoningResolution::RouteDefault
        } else {
            ReasoningResolution::Explicit
        };

        // Headroom and overflow both follow production's message/system
        // estimator. When an earlier runtime transform cannot be observed
        // without mutation, `runtime_transforms` makes this body unavailable
        // rather than publishing a guess.
        let input_budget_ceiling_tokens =
            context_input_budget_for_route(provider, &model, limits, 0);
        if crate::request_manifest::production_input_budget_exceeded(
            input_budget_ceiling_tokens,
            production_input_estimate_tokens,
        ) {
            runtime_transforms
                .push("context-overflow recovery would trim or compact the conversation");
        }

        let route_facts = RouteFacts {
            provider_id: SafeLabel::identifier(&prepared.endpoint.provider_id),
            provider_display: SafeLabel::phrase(&prepared.endpoint.provider_display),
            route_id: prepared
                .endpoint
                .route_id
                .as_deref()
                .map(SafeLabel::identifier),
            dialect: prepared.dialect.as_str().to_string(),
            route_shape: prepared.endpoint.shape.as_str().to_string(),
            endpoint_host_class: prepared.safe_endpoint_host_class(),
            endpoint_fingerprint: prepared.endpoint_fingerprint(),
            wire_model: SafeLabel::catalog_model(&prepared.wire_model),
            caller_entrypoint: prepared.entrypoint.as_str().to_string(),
            body_stream_field: prepared.wire_stream_field(),
            context_limit_tokens: route.context_window.tokens,
            context_limit_source: route.context_window.source,
            route_input_limit_tokens: limits.and_then(|limits| limits.input_tokens),
            route_output_limit_tokens: limits.and_then(|limits| limits.output_tokens),
            billing: preview_billing_facts(&route.config, &route.identity, &base_url),
            routing_source: routing_source.label().to_string(),
            auto_route_source: auto_route_source.as_deref().map(SafeLabel::phrase),
        };

        let prompt = self.preview_prompt_provenance(&prepared, system_prompt_text.as_str(), &model);

        // The body is a *dependent* fact. A tool surface whose MCP
        // contribution is unknown does not yield "the same body with no MCP
        // tools" — a real turn would connect and may send a different tool
        // list, a different tool region, and therefore a different body,
        // local component fingerprint, and hash. Publishing an exact body there was the reviewed
        // defect: it fabricated an empty MCP contribution and hashed it.
        // Likewise, a request the turn loop would rewrite before sending is
        // not the request that would be sent.
        let body = if let Some(inherited) = tools.propagate() {
            inherited
        } else if runtime_transforms.is_empty() {
            Availability::Exact(PreparedBodyInputs {
                prepared: &prepared,
                reasoning_resolution,
                prompt,
                input_budget_ceiling_tokens,
                production_input_estimate_tokens,
                tool_surface_is_exact: true,
            })
        } else {
            Availability::unavailable_with(
                UnavailableReason::RuntimeTransformsBeforeSend,
                runtime_transforms.join("; "),
            )
        };

        RequestManifest::build(ManifestDraft {
            session,
            route: Availability::Exact(route_facts),
            tools,
            body,
        })
    }

    /// Transforms the turn loop would apply to this conversation between
    /// dispatch and the first provider request.
    ///
    /// Every check is **read-only**. Nothing here drains the steer channel,
    /// receives a queued sub-agent completion, flushes an LSP block, or runs
    /// compaction: an inspection that consumed pending state would change the
    /// very turn it claims to describe. Where a queue can only be *counted*
    /// rather than inspected, counting is what happens.
    ///
    /// Returned strings are compile-time constants. They are joined into a
    /// typed unavailable detail, which still crosses the safe-label boundary.
    async fn preview_runtime_transforms(
        &self,
        messages: &[Message],
        system_prompt: Option<&SystemPrompt>,
        compaction: &crate::compaction::CompactionConfig,
    ) -> Vec<&'static str> {
        let mut reasons = Vec::new();
        if crate::profile_constitution::account_is_present(
            self.api_config.account_profile.as_deref(),
        )
        .unwrap_or(true)
            || self
                .constitution_block
                .as_deref()
                .is_some_and(|block| block.starts_with("Account profile constitution,"))
        {
            reasons.push("the account constitution is resolved at next-turn admission; preview its guidance in account settings");
        } else if crate::prompts::load_user_constitution_block() != self.constitution_block {
            reasons
                .push("the local constitution changed and will be recorded at next-turn admission");
        }

        if !self.pending_lsp_blocks.is_empty() {
            reasons.push("pending LSP diagnostics would be injected as a synthetic message");
        }

        let shell_completion_may_be_injected = self.shell_manager.lock().map_or(true, |manager| {
            manager.may_have_undelivered_completion_for_session(&self.session.id)
        });
        if shell_completion_may_be_injected {
            reasons.push("a background shell completion may be injected before the request");
        }

        let queued_completions = !self.rx_subagent_completion.is_empty() || {
            let manager = self.subagent_manager.read().await;
            manager.may_transform_next_parent_request_for_session(
                &self.session.id,
                &self.delivered_subagent_completion_ids,
            )
        };
        if queued_completions {
            reasons.push("a running or undelivered sub-agent completion may be injected");
        }

        if crate::compaction::compaction_pressure_reached(messages, system_prompt, compaction) {
            let prepared = self.prepare_compaction_envelope(compaction.clone());
            if should_compact(messages, system_prompt, &prepared) {
                reasons.push("making room would summarize the conversation first");
            }
        }

        reasons
    }

    /// Posture that depends on neither the route nor the next message.
    fn preview_session_facts(&self, inputs: &PreviewRequestInputs) -> SessionFacts {
        let base = crate::prompts::effective_base_prompt_text();
        let input_policy = effective_input_policy(
            inputs.provenance,
            inputs.mode,
            "",
            inputs.allow_shell,
            inputs.trust_mode,
            inputs.auto_approve,
            inputs.approval_mode,
        );
        SessionFacts {
            agent_role: "primary".to_string(),
            lane_kind: "interactive-primary".to_string(),
            fleet_assignment: "not-applicable-primary-agent".to_string(),
            requested_model: SafeLabel::catalog_model(&inputs.requested_model),
            auto_model_routing: inputs.auto_model,
            requested_reasoning: SafeLabel::identifier(&inputs.requested_reasoning),
            // What the caller supplied, not what planning managed to do with
            // it: a plan failure must not read as "you forgot `--prompt`".
            hypothetical_prompt_supplied: inputs.hypothetical_prompt_supplied,
            mode: input_policy.mode.label().to_string(),
            approval_mode: format!("{:?}", input_policy.approval_mode_for_session()),
            allowed_tool_gate_count: inputs.allowed_tools.as_ref().map(Vec::len),
            disallowed_tool_gate_count: self.config.disallowed_tools.as_ref().map(Vec::len),
            base_prompt: BasePromptProvenance {
                origin: crate::prompts::base_prompt_origin().label().to_string(),
                bytes: base.len(),
                sha256: crate::hashing::sha256_hex(base.as_bytes()),
            },
        }
    }

    /// System-prompt provenance, as labels and hashes only.
    ///
    /// `effective` is the prompt of the request being described, so an
    /// auto-routed preview classifies the prompt it would actually send
    /// rather than the session's currently installed one.
    fn preview_prompt_provenance(
        &self,
        prepared: &PreparedOutboundRequest,
        effective: &str,
        model: &str,
    ) -> PromptProvenance {
        let base = crate::prompts::effective_base_prompt_text();
        let configured = crate::prompts::compose_default_static_layers(model);

        let assembly = if effective.trim().is_empty() {
            SystemPromptAssembly::None
        } else if effective.trim() == base.trim() {
            SystemPromptAssembly::BaseOnly
        } else if effective.trim() == configured.trim() {
            SystemPromptAssembly::BaseWithConfiguredLayers
        } else {
            SystemPromptAssembly::BaseWithRuntimeAdditions
        };

        let view = prepared.wire_view();
        PromptProvenance {
            assembly,
            // The hash of the prompt the *prepared request* carries, in its
            // final wire form — not of an independently recomposed string.
            effective_system_canonical_json_bytes: view.system_bytes,
            effective_system_sha256: view.system_sha256.clone(),
        }
    }
}

/// Typed billing facts for the planned route, from the same helper the footer
/// and sidebar read. Every label is a compile-time constant.
fn preview_billing_facts(
    config: &crate::config::Config,
    identity: &crate::config::ProviderIdentity,
    base_url: &str,
) -> BillingFacts {
    if let Some(surface) =
        crate::pricing::billing_surface_for_route(identity.provider, Some(base_url))
    {
        return BillingFacts::Surface { surface };
    }
    match crate::route_billing::for_route(config, identity) {
        crate::route_billing::BillingPresentation::Metered => BillingFacts::Metered,
        crate::route_billing::BillingPresentation::Subscription(plan) => {
            BillingFacts::Subscription { plan }
        }
        crate::route_billing::BillingPresentation::Local => BillingFacts::Local,
        crate::route_billing::BillingPresentation::Unknown => BillingFacts::Unknown,
    }
}

/// Stable hash over the exact active tool catalog: name, description, and
/// schema, in catalog order. Changes when a tool is added, removed,
/// reordered, or has its schema transformed.
///
/// This is the *single* definition of the active-tool-catalog digest. The
/// request manifest fills `ToolSurfaceFacts::active_tool_catalog_sha256` from
/// it, and `crate::tool_inspection` reports the same value for the same
/// prepared request. Neither surface keeps a digest of its own, so a human
/// reading `/tools` and a human reading `/request` are looking at the same
/// accounting object rather than two hashes that can silently diverge.
pub(crate) fn active_tool_catalog_sha256(tools: &[Tool]) -> String {
    let mut canonical = String::new();
    for tool in tools {
        canonical.push_str(&tool.name);
        canonical.push('\u{1}');
        canonical.push_str(&tool.description);
        canonical.push('\u{1}');
        canonical.push_str(&crate::client::canonical_json(&tool.input_schema));
        canonical.push('\n');
    }
    crate::hashing::sha256_hex(canonical.as_bytes())
}

/// Whether the Standard and Full tool surfaces currently produce the same
/// catalog.
///
/// Derived, not asserted: the surface shaper is run over this exact catalog
/// under both budgets and the results compared. If Standard and Full ever
/// genuinely diverge, this reports `false` without anyone editing copy.
fn standard_and_full_collapse(
    catalog: &[Tool],
    always_load: &std::collections::HashSet<String>,
) -> bool {
    super::tool_catalog::surface_budgets_produce_same_catalog(
        catalog,
        always_load,
        crate::model_profile::ToolSurfaceBudget::Standard,
        crate::model_profile::ToolSurfaceBudget::Full,
    )
}

#[cfg(test)]
#[path = "preview/tests.rs"]
mod tests;

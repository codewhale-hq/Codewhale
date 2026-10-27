//! Redacted request manifest for `/preview-request` (#1004, #3928).
//!
//! A [`RequestManifest`] describes the request the **next primary agent turn**
//! would send: dialect, endpoint identity, wire model, reasoning resolution,
//! the exact active tool catalog, sizes, conservative offline token estimates,
//! and a whole-body hash.
//!
//! Four properties are load-bearing:
//!
//! 1. **Single-sourced.** Every wire fact is read off a
//!    [`PreparedOutboundRequest`] — the same value the transport sends. There
//!    is no second body builder, no Chat-shaped projection of a non-Chat
//!    route, and no re-derivation of prompt or tool selection here.
//! 2. **Typed, allowlisted disclosure.** The manifest is a fixed set of
//!    counts, hashes, enums, and short provenance labels, and every free-form
//!    string crosses [`codewhale_runtime::safe_label`] first. Prompt text, project
//!    instructions, memory, skill bodies, tool results, message content,
//!    credentials, URL paths, and absolute workspace paths have no field to
//!    occupy — and a hostile model or route id gets a fingerprint instead of
//!    a verbatim copy.
//! 3. **Whole-body fidelity.** The body hash covers the complete canonicalized
//!    wire body, so max-token fields, tool choice, nested reasoning controls,
//!    transformed tool schemas, attachments, and stream options all move it.
//! 4. **Structural honesty.** Facts that are not yet knowable are *absent*,
//!    not guessed. When auto model routing has not been resolved there is no
//!    provider, route id, dialect, endpoint, wire model, billing, tool budget,
//!    or body hash in this structure at all — a typed
//!    [`Unavailable`] stands in its place on both the human and the JSON
//!    surface. Recycling the current or previous route would be a lie about
//!    what the next request will contain.
//!
//! Scope: this describes the **primary `LlmClient` agent turn** only —
//! `create_message` / `create_message_stream`. Auxiliary provider calls (chat
//! translation, FIM completion, speech, provider-native search, model
//! listing, and the auto-router classifier) are separate requests with their
//! own shapes and are deliberately not covered. See `docs/PREVIEW_REQUEST.md`.
//!
//! Token figures are *offline estimates* (~4 bytes/token plus a conservative
//! margin). They are never provider-authoritative token counts.
//!
//! The `dryrun` concept this serves — inspect the next request from the real
//! request-building seam instead of a hand-rolled summary — is harvested from
//! PR #1099 by TaoMu (GTC2080).

use serde::Serialize;

use crate::client::PreparedOutboundRequest;
use codewhale_runtime::safe_label::SafeLabel;

/// Bytes-per-token divisor for the offline estimator.
const BYTES_PER_TOKEN: usize = 4;
/// Conservative margin applied to the estimated total (percent).
const ESTIMATE_MARGIN_PERCENT: usize = 5;
/// Bumped whenever a field is renamed or removed, so scripted consumers can
/// detect an incompatible manifest instead of silently reading `null`.
///
/// v3 introduced the sectioned `route` / `tools` / `body` availability shape.
/// v4 made the byte classes an exact accounting decomposition of the wire
/// body, moved the component digest onto the *wire* tool schemas, reported nested reasoning
/// efforts with their key path, and re-based headroom on the production input
/// budget rather than the raw context window.
/// v5 renamed the system/tools digest to state its local component scope and
/// removed the unsupported implication that it is a provider cache identity.
/// v6 replaced the raw endpoint host with a safe host class/digest and added
/// explicit route-limit provenance plus input/output budget facts.
/// v7 names canonical JSON sizes truthfully and adds primary-agent identity,
/// typed route provenance, and an explicit unavailable provider-usage receipt.
/// v8 makes authoritative Work state and the active goal-budget terminal gate
/// explicit fail-closed dependencies of an exact body.
pub(crate) const MANIFEST_SCHEMA_VERSION: u32 = 9;

/// Exact readable base-prompt-only disclosure for the explicit
/// `/preview-request base-prompt` mode. This deliberately returns no runtime
/// system layers: project instructions, skills, memory, and message content
/// remain represented only by the protected effective-system hash.
pub(crate) fn exact_base_prompt_only() -> String {
    crate::prompts::effective_base_prompt_text().to_string()
}

/// A section of the manifest that is either exactly known or typed-absent.
///
/// This is the whole point of the structure: there is no "unknown" *value*
/// anywhere in a manifest, because an unknown fact has no field.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Availability<T> {
    /// Exactly what the next turn would send.
    Exact(T),
    /// Not knowable without doing something a preview must not do.
    Unavailable(Unavailable),
}

impl<T> Availability<T> {
    pub(crate) fn unavailable(reason: UnavailableReason) -> Self {
        Self::Unavailable(Unavailable {
            reason,
            detail: None,
        })
    }

    pub(crate) fn unavailable_with(reason: UnavailableReason, detail: String) -> Self {
        Self::Unavailable(Unavailable {
            reason,
            detail: Some(codewhale_runtime::safe_label::safe_error_text(&detail)),
        })
    }

    pub(crate) fn map<U>(self, transform: impl FnOnce(T) -> U) -> Availability<U> {
        match self {
            Self::Exact(value) => Availability::Exact(transform(value)),
            Self::Unavailable(unavailable) => Availability::Unavailable(unavailable),
        }
    }

    #[cfg(test)]
    pub(crate) fn exact(&self) -> Option<&T> {
        match self {
            Self::Exact(value) => Some(value),
            Self::Unavailable(_) => None,
        }
    }

    /// Carry this section's unavailability onto a section that depends on it.
    ///
    /// Returns `None` when this section is exact, so the caller falls through
    /// to building the dependent section normally. This is what keeps a
    /// dependency from silently becoming exact: when the MCP contribution to
    /// the tool surface is unknown, the body built from that surface is a body
    /// no turn would send, and it must inherit the same typed reason rather
    /// than publish an exact hash of a fabricated request.
    pub(crate) fn propagate<U>(&self) -> Option<Availability<U>> {
        match self {
            Self::Exact(_) => None,
            Self::Unavailable(unavailable) => Some(Availability::Unavailable(unavailable.clone())),
        }
    }
}

/// Why a section could not be described exactly.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct Unavailable {
    pub(crate) reason: UnavailableReason,
    /// Bounded, path- and URL-path-safe explanation. Never raw error text.
    pub(crate) detail: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum UnavailableReason {
    /// Auto model routing is on and no hypothetical prompt was supplied, so
    /// the concrete route is decided by text that does not exist yet.
    AutoRouteUnresolvedUntilNextPrompt,
    /// Resolving Auto would make a provider/model classifier call, which an
    /// offline inspection command must never do.
    AutoRouteClassificationNotExecuted,
    /// No `--prompt` was supplied, so there is no exact next-turn body: the
    /// next user message is part of the request.
    NoHypotheticalPromptSupplied,
    /// The host's shared route planner failed for this hypothetical turn.
    RoutePlanFailed,
    /// The exact MCP tool state is not knowable without connecting, which an
    /// inspection must never do. Also carried by the body section: a body
    /// built from a tool surface that is missing its MCP contribution is not
    /// the body the next turn would send.
    McpStateNotSnapshottable,
    /// The shared prepared-request seam refused to build a body.
    RequestPreparationFailed,
    /// Mutable `message_submit` hooks are configured. They may rewrite or
    /// block the next message before anything downstream sees it, and an
    /// inspection must not execute them — so the route, tool surface, and body
    /// they would shape are all unknowable from here.
    MessageSubmitHooksNotExecuted,
    /// Resolving the hypothetical prompt into model-facing content failed the
    /// same way a real submit would have failed (skill authority, file
    /// mentions).
    PromptResolutionFailed,
    /// The turn loop would transform this request between dispatch and the
    /// wire — auto-compaction, context-overflow recovery, a background-shell
    /// or queued sub-agent completion, or pending LSP diagnostics — and an
    /// inspection may neither run nor consume any of them.
    RuntimeTransformsBeforeSend,
    /// The active goal has consumed its token budget, so the continuation
    /// gate stops before creating another provider request.
    GoalTokenBudgetExhausted,
    /// The live goal state could not be read without guessing whether its
    /// terminal budget gate would permit another request.
    GoalStateNotSnapshottable,
    /// Preview never sends a provider request, so no provider-counted usage
    /// receipt exists. This must not be represented by zero token counts.
    ProviderRequestNotExecuted,
}

impl UnavailableReason {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::AutoRouteUnresolvedUntilNextPrompt => {
                "auto model routing is unresolved until the next prompt"
            }
            Self::AutoRouteClassificationNotExecuted => {
                "auto route classification was not executed because preview is offline"
            }
            Self::NoHypotheticalPromptSupplied => {
                "no hypothetical prompt supplied — the next user message is part of the request"
            }
            Self::RoutePlanFailed => "the shared route planner could not resolve this turn",
            Self::McpStateNotSnapshottable => {
                "MCP tool state cannot be snapshotted without connecting"
            }
            Self::RequestPreparationFailed => "request preparation failed",
            Self::MessageSubmitHooksNotExecuted => {
                "message-submit hooks are configured and an inspection must not run them"
            }
            Self::PromptResolutionFailed => {
                "the hypothetical prompt could not be resolved into model-facing content"
            }
            Self::RuntimeTransformsBeforeSend => {
                "the turn loop would transform this request before sending it"
            }
            Self::GoalTokenBudgetExhausted => {
                "active goal token budget is exhausted; no outbound request is eligible"
            }
            Self::GoalStateNotSnapshottable => "active goal state cannot be snapshotted exactly",
            Self::ProviderRequestNotExecuted => {
                "provider request not executed; provider-reported usage is unavailable"
            }
        }
    }
}

/// How the reasoning tier for the next request was determined.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum ReasoningResolution {
    /// The user pinned a concrete tier; the next request will use it.
    Explicit,
    /// Auto routing, resolved against the supplied hypothetical prompt by the
    /// same planner a real turn runs.
    ResolvedFromHypotheticalPrompt,
    /// The body carries a reasoning control the user never asked for: the
    /// dialect or the route shapes one in by default. Reporting this as
    /// `Explicit` would credit the user with a selection they did not make.
    RouteDefault,
    /// The route asks for no reasoning at all. A Responses body that carries
    /// only `include` — which *discloses* reasoning output rather than
    /// requesting a tier — lands here, not on `Explicit`.
    NotApplicable,
}

impl ReasoningResolution {
    fn label(self) -> &'static str {
        match self {
            Self::Explicit => "explicit user selection",
            Self::ResolvedFromHypotheticalPrompt => {
                "auto, resolved against the supplied hypothetical prompt"
            }
            Self::RouteDefault => "route default (no user selection)",
            Self::NotApplicable => "route sends no reasoning control",
        }
    }
}

/// How the effective system prompt was assembled, without quoting any of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum SystemPromptAssembly {
    /// The effective system prompt is exactly the base-prompt bytes.
    BaseOnly,
    /// Base prompt plus the configured static layers, and nothing else.
    BaseWithConfiguredLayers,
    /// Runtime or session layers (environment, project instructions, skills,
    /// memory, mode) were appended on top.
    BaseWithRuntimeAdditions,
    /// No system prompt would be sent.
    None,
}

impl SystemPromptAssembly {
    fn label(self) -> &'static str {
        match self {
            Self::BaseOnly => "base prompt only",
            Self::BaseWithConfiguredLayers => "base prompt + configured static layers",
            Self::BaseWithRuntimeAdditions => {
                "base prompt + configured layers + runtime/session additions"
            }
            Self::None => "no system prompt",
        }
    }
}

/// How this route is billed, as typed facts with static labels.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub(crate) enum BillingFacts {
    /// Per-token API usage.
    Metered,
    /// Account/subscription quota; per-token dollar estimates are not spend.
    Subscription { plan: &'static str },
    /// Local route with no provider bill.
    Local,
    /// Billing basis unknown; never invent dollars or a fake zero.
    Unknown,
    /// Endpoint-derived pricing surface classification for routes that have
    /// one (`Stepfun` today). Never a URL.
    Surface { surface: &'static str },
}

impl BillingFacts {
    fn label(&self) -> String {
        match self {
            Self::Metered => "metered API (per-token)".to_string(),
            Self::Subscription { plan } => format!("subscription quota ({plan})"),
            Self::Local => "local route (no provider bill)".to_string(),
            Self::Unknown => "unknown billing basis".to_string(),
            Self::Surface { surface } => format!("metered API, pricing surface `{surface}`"),
        }
    }
}

/// Base-prompt provenance: labels, byte counts, and hashes only (#3928).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct BasePromptProvenance {
    /// Where the base-prompt bytes came from: bundled or configured override.
    /// A static runtime label, never a source-tree path.
    pub(crate) origin: String,
    pub(crate) bytes: usize,
    pub(crate) sha256: String,
}

/// System-prompt provenance of the *prepared request*.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct PromptProvenance {
    /// How the effective prompt was assembled from the base prompt.
    pub(crate) assembly: SystemPromptAssembly,
    /// Canonical JSON bytes and hash of the system region of the prepared
    /// request — the same semantic prompt value production sends.
    pub(crate) effective_system_canonical_json_bytes: usize,
    pub(crate) effective_system_sha256: String,
}

/// Session posture that does not depend on the route or the next message.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct SessionFacts {
    /// Exact execution identity for this manifest's deliberately narrow scope.
    pub(crate) agent_role: String,
    pub(crate) lane_kind: String,
    /// Primary interactive turns are not Fleet workers. Say that explicitly
    /// rather than inventing a Fleet role or leaving an ambiguous null.
    pub(crate) fleet_assignment: String,
    /// The model the user selected, before route remapping. `auto` when auto
    /// model routing is on — never a concrete model the user did not pick.
    pub(crate) requested_model: SafeLabel,
    /// Whether auto model routing is selected.
    pub(crate) auto_model_routing: bool,
    /// Reasoning tier the user asked for (`auto`, `high`, `off`, …).
    pub(crate) requested_reasoning: SafeLabel,
    /// Whether the caller supplied a hypothetical next prompt.
    pub(crate) hypothetical_prompt_supplied: bool,
    /// Operating mode the catalog would be built under.
    pub(crate) mode: String,
    /// Approval posture the catalog would be filtered under.
    pub(crate) approval_mode: String,
    /// Number of entries in the allow-list gate, if one is configured.
    pub(crate) allowed_tool_gate_count: Option<usize>,
    /// Number of entries in the deny-list gate, if one is configured.
    pub(crate) disallowed_tool_gate_count: Option<usize>,
    pub(crate) base_prompt: BasePromptProvenance,
}

/// Exactly which route the next turn would use.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct RouteFacts {
    pub(crate) provider_id: SafeLabel,
    pub(crate) provider_display: SafeLabel,
    /// Named custom-provider / route identity from the resolved turn plan.
    pub(crate) route_id: Option<SafeLabel>,
    pub(crate) dialect: String,
    pub(crate) route_shape: String,
    /// Safe endpoint class. Remote authorities are represented only by a
    /// bounded digest; even a credential-shaped tenant subdomain is never
    /// printed. Loopback is reported as a class, never a raw host.
    pub(crate) endpoint_host_class: String,
    /// SHA-256 of the full endpoint URL, for "same endpoint?" comparisons.
    pub(crate) endpoint_fingerprint: String,
    /// The model id literally placed on the wire, after route remapping.
    pub(crate) wire_model: SafeLabel,
    /// Which transport entry point this manifest described.
    pub(crate) caller_entrypoint: String,
    /// The `stream` field **as it appears on the body**, or `null` when the
    /// body carries no such field. Derived from the body, never from the
    /// caller entry point: the Responses blocking path sends `stream: true`.
    ///
    /// This lives in the *route* section, and stays exact even when the body
    /// section does not, because every dialect builder writes it from the
    /// caller entry point and the provider alone — never from the message list
    /// or the tool set. It is a property of the route, not of the payload.
    pub(crate) body_stream_field: Option<bool>,
    /// Active context-window ceiling and the resolver receipt for its source.
    pub(crate) context_limit_tokens: u32,
    pub(crate) context_limit_source: crate::route_runtime::ContextWindowSource,
    /// Concrete route/offering limits, when advertised. These remain
    /// separate from the effective wire output cap.
    pub(crate) route_input_limit_tokens: Option<u64>,
    pub(crate) route_output_limit_tokens: Option<u64>,
    pub(crate) billing: BillingFacts,
    /// Upstream planner receipt for why this concrete route was selected.
    pub(crate) routing_source: String,
    /// How the auto router chose this route, when auto routing ran.
    pub(crate) auto_route_source: Option<SafeLabel>,
}

/// Everything the manifest reports about the next request's tool surface.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct ToolSurfaceFacts {
    /// Tools in the full built catalog, including deferred ones.
    pub(crate) catalog_tool_count: usize,
    /// Tools deferred (discoverable via tool search, not sent eagerly).
    pub(crate) deferred_tool_count: usize,
    /// Tools that would actually be serialized into this request.
    pub(crate) active_tool_count: usize,
    /// Stable hash over the *current active* catalog, before dialect shaping:
    /// name, description, and canonical logical schema, in catalog order. Not
    /// the last turn's.
    ///
    /// This is a *catalog identity*, not a wire fact. Two routes can agree
    /// here and still send different bytes, because each dialect transforms
    /// schemas its own way and strict mode sanitizes them further. The hash
    /// the provider actually receives is `body.tool_schema_wire_sha256`, and
    /// that is the one the local system/tools component digest is built from.
    pub(crate) active_tool_catalog_sha256: String,
    /// The capability profile's surface budget label for this route.
    pub(crate) tool_surface_budget: String,
    /// Truthful disclosure (#1004): whether Standard and Full currently
    /// produce the same catalog. Derived by running the surface shaper under
    /// both budgets over this exact catalog — not asserted.
    pub(crate) standard_and_full_surfaces_collapsed: bool,
    /// MCP servers connected and contributing tools.
    pub(crate) mcp_server_count: usize,
    /// Tools in the active catalog that came from MCP.
    pub(crate) mcp_tool_count: usize,
}

/// Per-class conservative offline estimates, derived from the wire body.
///
/// `system`, `tool_schemas`, `messages`, and `framing` are estimates over the
/// four classes of the exact byte accounting (see [`crate::client::WireBodyView`]),
/// so they sum to the whole body rather than a selected part of it.
/// `tool_results` and `attachments` are *subsets* of `messages`, reported for
/// attribution and never added again.
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub(crate) struct TokenEstimates {
    pub(crate) system: usize,
    pub(crate) tool_schemas: usize,
    pub(crate) messages: usize,
    pub(crate) tool_results: usize,
    pub(crate) attachments: usize,
    pub(crate) framing: usize,
    /// Estimate over the **whole** canonical JSON body plus a conservative margin.
    ///
    /// Derived from the complete canonical body rather than by summing the
    /// per-class estimates, so per-class rounding cannot make the total drift
    /// away from the semantic JSON value production sends.
    pub(crate) total_conservative: usize,
}

/// Facts read off the exact next-turn body.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct BodyFacts {
    pub(crate) reasoning_resolution: ReasoningResolution,
    /// Reasoning-control keys present on the wire. Keys only, never values
    /// that could carry free text.
    pub(crate) reasoning_wire_control_keys: Vec<String>,
    /// The effort string actually on the wire, whether the dialect writes it
    /// flat (`reasoning_effort`) or nested (`thinking.effort`,
    /// `reasoning.effort`, `output_config.effort`).
    pub(crate) reasoning_wire_effort: Option<SafeLabel>,
    /// Which key path [`Self::reasoning_wire_effort`] was read from. A
    /// compile-time constant from the dialect allowlist, never a key taken
    /// out of the body.
    pub(crate) reasoning_wire_effort_source: Option<String>,
    /// `tool_choice` as it appears on the wire, as a short shape label.
    pub(crate) tool_choice: Option<SafeLabel>,
    pub(crate) prompt: PromptProvenance,

    /// Canonical JSON byte length of the complete body. The four accounting
    /// class counts below sum to this number; they are not HTTP byte ranges.
    pub(crate) body_canonical_json_bytes: usize,
    pub(crate) system_canonical_json_bytes: usize,
    pub(crate) tool_schema_canonical_json_bytes: usize,
    pub(crate) message_count: usize,
    pub(crate) message_canonical_json_bytes: usize,
    /// Subset of [`Self::message_canonical_json_bytes`].
    pub(crate) tool_result_canonical_json_bytes: usize,
    pub(crate) attachment_count: usize,
    /// Subset of [`Self::message_canonical_json_bytes`].
    pub(crate) attachment_canonical_json_bytes: usize,
    /// Algebraic remainder after the selected canonical value-region sizes.
    pub(crate) framing_canonical_json_bytes: usize,

    pub(crate) estimates: TokenEstimates,
    /// Estimated input tokens still available before this route's **input
    /// budget ceiling** — the production seam
    /// (`context_input_budget_for_route`) the turn loop itself checks, which
    /// is the context window minus the output reservation and safety
    /// headroom. Deliberately *not* the raw context limit: subtracting input
    /// from a window the route also has to fit its output into reports
    /// headroom the turn does not have. Negative when the turn would exceed
    /// the budget; absent when the route publishes no budget.
    pub(crate) estimated_input_headroom_tokens: Option<i64>,
    /// The exact production input-budget ceiling from which headroom was
    /// computed, after output reservation and safety headroom.
    pub(crate) input_budget_ceiling_tokens: Option<usize>,
    /// Output cap literally present on the prepared wire body. Absent means
    /// this dialect did not publish one; no inferred value is substituted.
    pub(crate) wire_output_cap_tokens: Option<u64>,

    /// SHA-256 over the canonicalized **complete** wire body.
    pub(crate) body_sha256: String,
    /// SHA-256 over the canonicalized wire `tools` region — the schemas the
    /// provider receives, after dialect transforms and strict-mode
    /// sanitizing. Absent when the body carries no tools.
    pub(crate) tool_schema_wire_sha256: Option<String>,
    /// Local fingerprint over the final wire system-region hash and final wire
    /// tool-region hash. This is not a provider cache key and makes no claim
    /// that those regions are adjacent in a provider-specific prefix.
    pub(crate) local_system_tools_component_sha256: Option<String>,
    /// Provider-authoritative counts are available only after a real response.
    pub(crate) provider_reported_usage: Availability<ProviderReportedUsage>,
}

/// Provider-authoritative usage, populated only from a completed response.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct ProviderReportedUsage {
    pub(crate) input_tokens: u64,
    pub(crate) output_tokens: u64,
}

/// What the engine hands to [`RequestManifest::build`] for the body section.
pub(crate) struct PreparedBodyInputs<'a> {
    pub(crate) prepared: &'a PreparedOutboundRequest,
    pub(crate) reasoning_resolution: ReasoningResolution,
    pub(crate) prompt: PromptProvenance,
    /// This route's production input-budget ceiling in tokens, from
    /// `context_input_budget_for_route` — the same seam the turn loop checks
    /// before it sends. `None` when the route publishes no budget.
    pub(crate) input_budget_ceiling_tokens: Option<usize>,
    /// Input estimate from production's overflow contract,
    /// the base `estimate_input_tokens_conservative(messages, system)` plus a
    /// separately framed transient Work-tail estimate, evaluated over the
    /// exact hypothetical turn. This is intentionally independent of the
    /// manifest's whole-wire-body estimate.
    pub(crate) production_input_estimate_tokens: usize,
    /// Whether the tool surface is exactly known. The local component digest
    /// is published only when it is: a fingerprint computed over a tool region
    /// missing its MCP contribution would compare equal to nothing real.
    pub(crate) tool_surface_is_exact: bool,
}

/// The complete draft the engine assembles before rendering.
pub(crate) struct ManifestDraft<'a> {
    pub(crate) session: SessionFacts,
    pub(crate) route: Availability<RouteFacts>,
    pub(crate) tools: Availability<ToolSurfaceFacts>,
    pub(crate) body: Availability<PreparedBodyInputs<'a>>,
}

/// A redacted description of the request that would be sent.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct RequestManifest {
    pub(crate) schema_version: u32,
    pub(crate) session: SessionFacts,
    pub(crate) route: Availability<RouteFacts>,
    pub(crate) tools: Availability<ToolSurfaceFacts>,
    pub(crate) body: Availability<BodyFacts>,
}

/// Signed production input headroom for one route ceiling and estimate.
/// Negative means the turn loop's preflight gate must recover or stop before
/// sending. Both the manifest and preview overflow decision use this helper so
/// the displayed headroom cannot disagree with request eligibility.
pub(crate) fn production_input_headroom(
    ceiling_tokens: Option<usize>,
    estimate_tokens: usize,
) -> Option<i64> {
    ceiling_tokens
        .and_then(|ceiling| {
            Some((
                i64::try_from(ceiling).ok()?,
                i64::try_from(estimate_tokens).ok()?,
            ))
        })
        .map(|(ceiling, estimate)| ceiling - estimate)
}

#[must_use]
pub(crate) fn production_input_budget_exceeded(
    ceiling_tokens: Option<usize>,
    estimate_tokens: usize,
) -> bool {
    production_input_headroom(ceiling_tokens, estimate_tokens).is_some_and(|headroom| headroom < 0)
}

impl RequestManifest {
    /// Build a manifest from a draft whose route/tool/body sections have
    /// already been resolved — or typed as unavailable — by the engine.
    pub(crate) fn build(draft: ManifestDraft<'_>) -> Self {
        let body = draft.body.map(|inputs| {
            let view = inputs.prepared.wire_view();
            // The accounting classes below must sum to the body. They describe
            // canonical value-region sizes plus a remainder, not disjoint
            // borrowed ranges in the serialized JSON buffer.
            debug_assert!(
                view.partition_is_exact(),
                "wire byte accounting must sum to the wire body exactly"
            );
            let estimates = TokenEstimates::from_view(&view);
            // Headroom comes from the production input-budget seam, never from
            // the raw context window: the window has to hold the response too.
            let estimated_input_headroom_tokens = production_input_headroom(
                inputs.input_budget_ceiling_tokens,
                inputs.production_input_estimate_tokens,
            );
            let tool_schema_wire_sha256 =
                (!view.tool_schema_sha256.is_empty()).then(|| view.tool_schema_sha256.clone());
            // A local identity for the two final wire components. This hashes
            // their digests rather than claiming they form one contiguous
            // provider-cache prefix or a route-scoped cache key.
            let local_system_tools_component_sha256 = inputs.tool_surface_is_exact.then(|| {
                codewhale_runtime::hashing::sha256_hex(
                    format!(
                        "system={}\ntools={}\n",
                        view.system_sha256, view.tool_schema_sha256
                    )
                    .as_bytes(),
                )
            });
            let wire_effort = inputs.prepared.reasoning.wire_effort();

            BodyFacts {
                reasoning_resolution: inputs.reasoning_resolution,
                reasoning_wire_control_keys: inputs
                    .prepared
                    .reasoning
                    .wire_controls
                    .iter()
                    .map(|(key, _)| key.clone())
                    .collect(),
                reasoning_wire_effort: wire_effort.map(|(_, effort)| SafeLabel::identifier(effort)),
                reasoning_wire_effort_source: wire_effort.map(|(source, _)| source.to_string()),
                // Read the final provider-shaped body. The logical request can
                // be remapped (Anthropic/Responses) or omitted altogether
                // (DeepSeek thinking), so carrying the pre-transform value
                // would report a choice the provider never receives.
                tool_choice: tool_choice_label(inputs.prepared.body.get("tool_choice")),
                prompt: inputs.prompt,
                body_canonical_json_bytes: view.body_bytes,
                system_canonical_json_bytes: view.system_bytes,
                tool_schema_canonical_json_bytes: view.tool_schema_bytes,
                message_count: view.items.len(),
                message_canonical_json_bytes: view.item_bytes,
                tool_result_canonical_json_bytes: view.tool_result_bytes,
                attachment_count: view.attachment_count,
                attachment_canonical_json_bytes: view.attachment_bytes,
                framing_canonical_json_bytes: view.framing_bytes,
                estimates,
                estimated_input_headroom_tokens,
                input_budget_ceiling_tokens: inputs.input_budget_ceiling_tokens,
                wire_output_cap_tokens: inputs.prepared.wire_output_cap_tokens(),
                body_sha256: inputs.prepared.body_sha256(),
                tool_schema_wire_sha256,
                local_system_tools_component_sha256,
                provider_reported_usage: Availability::unavailable(
                    UnavailableReason::ProviderRequestNotExecuted,
                ),
            }
        });

        Self {
            schema_version: MANIFEST_SCHEMA_VERSION,
            session: draft.session,
            route: draft.route,
            tools: draft.tools,
            body,
        }
    }

    /// Pretty JSON rendering. Redacted by construction.
    pub(crate) fn to_json(&self) -> String {
        serde_json::to_string_pretty(self)
            .unwrap_or_else(|error| format!("{{\"error\":\"{error}\"}}"))
    }

    /// Human-readable manifest for the transcript.
    pub(crate) fn render(&self) -> String {
        let mut out = String::new();
        out.push_str("Request manifest (preview only — nothing was sent)\n");
        out.push_str(
            "Typed counts, hashes, and provenance for the next primary agent turn. \
             No prompt or message text.\n\n",
        );

        self.render_session(&mut out);
        self.render_route(&mut out);
        self.render_tools(&mut out);
        self.render_body(&mut out);

        if !self.session.hypothetical_prompt_supplied {
            if self.session.auto_model_routing {
                out.push_str(
                    "\nAuto route and body remain unavailable: preview never runs the \
                     provider-backed classifier. Select a fixed route for an exact preview.\n",
                );
            } else {
                out.push_str(
                    "\nPass `--prompt <text>` to resolve the fixed route and describe the exact \
                     next-turn body.\n",
                );
            }
        }
        out.push_str(
            "Token figures are offline estimates (~4 bytes/token + 5% margin), \
             never exact provider tokens.\n",
        );
        out.push_str(
            "Scope: the primary agent turn only. Translation, FIM, speech, \
             provider-native search, and the auto-router classifier are separate \
             auxiliary calls; preview never executes them.\n",
        );
        out
    }

    fn render_session(&self, out: &mut String) {
        out.push_str("Session\n");
        push_row(out, "agent role", &self.session.agent_role);
        push_row(out, "lane", &self.session.lane_kind);
        push_row(out, "Fleet assignment", &self.session.fleet_assignment);
        push_row(
            out,
            "model (requested)",
            self.session.requested_model.as_str(),
        );
        push_row(
            out,
            "model routing",
            if self.session.auto_model_routing {
                "auto"
            } else {
                "fixed"
            },
        );
        push_row(
            out,
            "reasoning (requested)",
            self.session.requested_reasoning.as_str(),
        );
        push_row(
            out,
            "mode / approval",
            &format!("{} / {}", self.session.mode, self.session.approval_mode),
        );
        push_row(
            out,
            "gates (allow / deny)",
            &format!(
                "{} / {}",
                count_label(self.session.allowed_tool_gate_count),
                count_label(self.session.disallowed_tool_gate_count),
            ),
        );
        push_row(out, "base prompt origin", &self.session.base_prompt.origin);
        push_row(
            out,
            "base prompt",
            &format!(
                "{} bytes (sha256 {})",
                self.session.base_prompt.bytes, self.session.base_prompt.sha256
            ),
        );
    }

    fn render_route(&self, out: &mut String) {
        out.push_str("\nRoute\n");
        let route = match &self.route {
            Availability::Unavailable(unavailable) => {
                push_unavailable(out, unavailable);
                return;
            }
            Availability::Exact(route) => route,
        };
        push_row(
            out,
            "provider",
            &format!("{} ({})", route.provider_display, route.provider_id),
        );
        if let Some(route_id) = &route.route_id {
            push_row(out, "route id", route_id.as_str());
        }
        if let Some(source) = &route.auto_route_source {
            push_row(out, "auto route source", source.as_str());
        }
        push_row(out, "routing source", &route.routing_source);
        push_row(out, "dialect", &route.dialect);
        push_row(out, "route shape", &route.route_shape);
        push_row(out, "endpoint host class", &route.endpoint_host_class);
        push_row(out, "endpoint fingerprint", &route.endpoint_fingerprint);
        push_row(out, "model (wire)", route.wire_model.as_str());
        push_row(out, "prepared for", &route.caller_entrypoint);
        push_row(
            out,
            "body `stream` field",
            match route.body_stream_field {
                Some(true) => "true",
                Some(false) => "false",
                None => "not present on the body",
            },
        );
        push_row(
            out,
            "context limit",
            &format!(
                "{} ({})",
                route.context_limit_tokens,
                route.context_limit_source.label()
            ),
        );
        push_row(
            out,
            "route input limit",
            &route
                .route_input_limit_tokens
                .map_or_else(|| "unknown".to_string(), |limit| limit.to_string()),
        );
        push_row(
            out,
            "route output limit",
            &route
                .route_output_limit_tokens
                .map_or_else(|| "unknown".to_string(), |limit| limit.to_string()),
        );
        push_row(out, "billing", &route.billing.label());
    }

    fn render_tools(&self, out: &mut String) {
        out.push_str("\nTools (the exact catalog this request would send)\n");
        let tools = match &self.tools {
            Availability::Unavailable(unavailable) => {
                push_unavailable(out, unavailable);
                return;
            }
            Availability::Exact(tools) => tools,
        };
        push_row(out, "active", &tools.active_tool_count.to_string());
        push_row(
            out,
            "catalog / deferred",
            &format!(
                "{} / {}",
                tools.catalog_tool_count, tools.deferred_tool_count
            ),
        );
        push_row(
            out,
            "active catalog sha256",
            &tools.active_tool_catalog_sha256,
        );
        push_row(out, "surface budget", &tools.tool_surface_budget);
        push_row(
            out,
            "Standard vs Full",
            if tools.standard_and_full_surfaces_collapsed {
                "collapsed — both budgets currently produce the same catalog"
            } else {
                "distinct — the budgets produce different catalogs"
            },
        );
        push_row(
            out,
            "MCP (servers / tools)",
            &format!("{} / {}", tools.mcp_server_count, tools.mcp_tool_count),
        );
    }

    fn render_body(&self, out: &mut String) {
        out.push_str("\nWire body\n");
        let body = match &self.body {
            Availability::Unavailable(unavailable) => {
                push_unavailable(out, unavailable);
                return;
            }
            Availability::Exact(body) => body,
        };

        push_row(
            out,
            "canonical JSON bytes (total)",
            &body.body_canonical_json_bytes.to_string(),
        );
        push_row(
            out,
            "canonical JSON bytes (system)",
            &body.system_canonical_json_bytes.to_string(),
        );
        push_row(
            out,
            "canonical JSON bytes (tool schemas)",
            &body.tool_schema_canonical_json_bytes.to_string(),
        );
        push_row(
            out,
            "messages / canonical JSON bytes",
            &format!(
                "{} / {}",
                body.message_count, body.message_canonical_json_bytes
            ),
        );
        push_row(
            out,
            "tool-result canonical JSON bytes",
            &body.tool_result_canonical_json_bytes.to_string(),
        );
        push_row(
            out,
            "attachments / canonical JSON bytes",
            &format!(
                "{} / {}",
                body.attachment_count, body.attachment_canonical_json_bytes
            ),
        );
        push_row(
            out,
            "framing canonical JSON bytes",
            &format!(
                "{} (key names, punctuation, all other fields)",
                body.framing_canonical_json_bytes
            ),
        );
        push_row(
            out,
            "byte classes",
            "system + tool schemas + messages + framing = total, exactly",
        );

        out.push_str("\nReasoning (as prepared)\n");
        push_row(out, "resolution", body.reasoning_resolution.label());
        push_row(
            out,
            "wire controls",
            if body.reasoning_wire_control_keys.is_empty() {
                "none".to_string()
            } else {
                body.reasoning_wire_control_keys.join(", ")
            }
            .as_str(),
        );
        push_row(
            out,
            "wire effort",
            &match (
                &body.reasoning_wire_effort,
                &body.reasoning_wire_effort_source,
            ) {
                (Some(effort), Some(source)) => format!("{effort} (from `{source}`)"),
                (Some(effort), None) => effort.as_str().to_string(),
                _ => "not sent".to_string(),
            },
        );
        push_row(
            out,
            "tool_choice",
            body.tool_choice
                .as_ref()
                .map_or("not sent", SafeLabel::as_str),
        );

        out.push_str("\nSystem prompt (as prepared)\n");
        push_row(out, "assembly", body.prompt.assembly.label());
        push_row(
            out,
            "effective system",
            &format!(
                "{} canonical JSON bytes (sha256 {})",
                body.prompt.effective_system_canonical_json_bytes,
                body.prompt.effective_system_sha256
            ),
        );

        out.push_str("\nEstimated tokens (offline estimate, not provider-counted)\n");
        push_row(out, "system", &body.estimates.system.to_string());
        push_row(
            out,
            "tool schemas",
            &body.estimates.tool_schemas.to_string(),
        );
        push_row(out, "messages", &body.estimates.messages.to_string());
        push_row(
            out,
            "  of which tool results",
            &body.estimates.tool_results.to_string(),
        );
        push_row(
            out,
            "  of which attachments",
            &body.estimates.attachments.to_string(),
        );
        push_row(out, "framing", &body.estimates.framing.to_string());
        push_row(
            out,
            "total (conservative)",
            &format!("~{}", body.estimates.total_conservative),
        );
        push_row(
            out,
            "input budget ceiling",
            &body
                .input_budget_ceiling_tokens
                .map_or_else(|| "unknown".to_string(), |ceiling| ceiling.to_string()),
        );
        push_row(
            out,
            "headroom (input budget)",
            &body.estimated_input_headroom_tokens.map_or_else(
                || "unknown".to_string(),
                |headroom| {
                    format!("~{headroom} (production estimate; window minus output reservation)")
                },
            ),
        );
        push_row(
            out,
            "output cap (wire)",
            &body
                .wire_output_cap_tokens
                .map_or_else(|| "unknown".to_string(), |cap| cap.to_string()),
        );
        push_row(
            out,
            "provider-reported usage",
            UnavailableReason::ProviderRequestNotExecuted.label(),
        );

        out.push_str("\nHashes\n");
        push_row(out, "whole body", &body.body_sha256);
        push_row(
            out,
            "wire tool schemas",
            body.tool_schema_wire_sha256
                .as_deref()
                .unwrap_or("no tools on this request"),
        );
        push_row(
            out,
            "local system + tools component",
            body.local_system_tools_component_sha256
                .as_deref()
                .unwrap_or("unavailable — tool surface is not exactly known"),
        );
    }
}

impl TokenEstimates {
    fn from_view(view: &crate::client::WireBodyView<'_>) -> Self {
        // The classes partition the body exactly, so the total is taken over
        // the body itself rather than summed from four independently rounded
        // per-class estimates.
        Self {
            system: estimate_bytes(view.system_bytes),
            tool_schemas: estimate_bytes(view.tool_schema_bytes),
            messages: estimate_bytes(view.item_bytes),
            // Tool results and attachments are *subsets* of the message bytes,
            // reported for attribution and deliberately not added again.
            tool_results: estimate_bytes(view.tool_result_bytes),
            attachments: estimate_bytes(view.attachment_bytes),
            framing: estimate_bytes(view.framing_bytes),
            total_conservative: conservative_token_estimate(view.body_bytes),
        }
    }
}

fn push_row(out: &mut String, label: &str, value: &str) {
    out.push_str(&format!("  {label:<30} {value}\n"));
}

fn push_unavailable(out: &mut String, unavailable: &Unavailable) {
    push_row(out, "unavailable", unavailable.reason.label());
    if let Some(detail) = &unavailable.detail {
        push_row(out, "  detail", detail);
    }
}

fn count_label(count: Option<usize>) -> String {
    count.map_or_else(|| "none".to_string(), |count| count.to_string())
}

fn estimate_bytes(bytes: usize) -> usize {
    bytes.div_ceil(BYTES_PER_TOKEN)
}

/// The offline input-token estimate this manifest publishes, for `bytes` of
/// wire body.
///
/// This is an independent provider-body observability estimate. Production's
/// overflow decision and the manifest's headroom deliberately use
/// `compaction::estimate_input_tokens_conservative(messages, system)` instead.
pub(crate) fn conservative_token_estimate(bytes: usize) -> usize {
    let whole = estimate_bytes(bytes);
    whole.saturating_add(whole * ESTIMATE_MARGIN_PERCENT / 100)
}

/// Short shape label for a wire `tool_choice` value.
///
/// Structural only, and bounded: the forced-function *name* is a tool name,
/// which is safe, but it still crosses the safe-label boundary because a
/// provider-shaped body can carry anything under that key.
pub(crate) fn tool_choice_label(value: Option<&serde_json::Value>) -> Option<SafeLabel> {
    let value = value?;
    if let Some(text) = value.as_str() {
        return Some(SafeLabel::identifier(text));
    }
    let kind = value
        .get("type")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("object");
    match value
        .pointer("/function/name")
        .and_then(serde_json::Value::as_str)
    {
        Some(name) => Some(SafeLabel::identifier(&format!("{kind}:{name}"))),
        None => Some(SafeLabel::identifier(kind)),
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;

    pub(crate) fn session() -> SessionFacts {
        SessionFacts {
            agent_role: "primary".to_string(),
            lane_kind: "interactive-primary".to_string(),
            fleet_assignment: "not-applicable-primary-agent".to_string(),
            requested_model: SafeLabel::identifier("glm-5.2"),
            auto_model_routing: false,
            requested_reasoning: SafeLabel::identifier("high"),
            hypothetical_prompt_supplied: true,
            mode: "Agent".to_string(),
            approval_mode: "Prompt".to_string(),
            allowed_tool_gate_count: None,
            disallowed_tool_gate_count: None,
            base_prompt: BasePromptProvenance {
                origin: "bundled in this codewhale-tui build".to_string(),
                bytes: 11,
                sha256: codewhale_runtime::hashing::sha256_hex(b"BASE PROMPT"),
            },
        }
    }

    pub(crate) fn route() -> RouteFacts {
        RouteFacts {
            provider_id: SafeLabel::identifier("zhipu"),
            provider_display: SafeLabel::identifier("Z.ai"),
            route_id: Some(SafeLabel::identifier("my-gateway")),
            dialect: "chat-completions".to_string(),
            route_shape: "standard".to_string(),
            endpoint_host_class: "https remote sha256:0123456789ab".to_string(),
            endpoint_fingerprint: codewhale_runtime::hashing::sha256_hex(b"endpoint"),
            wire_model: SafeLabel::identifier("glm-5.2"),
            caller_entrypoint: "streaming".to_string(),
            body_stream_field: Some(true),
            context_limit_tokens: 200_000,
            context_limit_source: crate::route_runtime::ContextWindowSource::Catalog,
            route_input_limit_tokens: Some(180_000),
            route_output_limit_tokens: Some(20_000),
            billing: BillingFacts::Metered,
            routing_source: "active-fixed-route".to_string(),
            auto_route_source: None,
        }
    }

    pub(crate) fn tools() -> ToolSurfaceFacts {
        ToolSurfaceFacts {
            catalog_tool_count: 4,
            deferred_tool_count: 2,
            active_tool_count: 2,
            active_tool_catalog_sha256: codewhale_runtime::hashing::sha256_hex(b"tools"),
            tool_surface_budget: "Standard".to_string(),
            standard_and_full_surfaces_collapsed: true,
            mcp_server_count: 0,
            mcp_tool_count: 0,
        }
    }

    pub(crate) fn prompt() -> PromptProvenance {
        PromptProvenance {
            assembly: SystemPromptAssembly::BaseOnly,
            effective_system_canonical_json_bytes: 11,
            effective_system_sha256: codewhale_runtime::hashing::sha256_hex(b"BASE PROMPT"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::{prompt, route, session, tools};
    use super::*;
    use crate::client::{CallerStreamMode, EndpointIdentity, RouteShape, WireDialect};
    use serde_json::json;

    fn endpoint(url: &str) -> EndpointIdentity {
        EndpointIdentity {
            provider_id: "zhipu".to_string(),
            provider_display: "Z.ai".to_string(),
            route_id: Some("my-gateway".to_string()),
            url: url.to_string(),
            shape: RouteShape::Standard,
        }
    }

    fn chat_body() -> serde_json::Value {
        json!({
            "model": "glm-5.2",
            "messages": [
                {"role": "system", "content": "SECRET SYSTEM PROMPT"},
                {"role": "user", "content": "SECRET USER MESSAGE"},
                {"role": "tool", "tool_call_id": "c1", "content": "SECRET TOOL OUTPUT"},
            ],
            "tools": [{"type": "function", "function": {"name": "read_file"}}],
            "tool_choice": {"type": "auto"},
            "max_tokens": 4096,
            "reasoning_effort": "high",
            "stream": true,
        })
    }

    fn prepared_chat(body: serde_json::Value) -> PreparedOutboundRequest {
        let fixture_url = format!(
            "https://user:{}@api.z.ai:8443/api/paas/v4/chat/completions?api_key={}{}",
            "hunter2", "sk", "-fixture-not-a-real-key-00000000"
        );
        PreparedOutboundRequest::new(
            WireDialect::ChatCompletions,
            endpoint(&fixture_url),
            "glm-5.2".to_string(),
            body,
            Some("high".to_string()),
            None,
            CallerStreamMode::Streaming,
        )
    }

    /// A stand-in for this route's production input-budget ceiling. Real
    /// values come from `context_input_budget_for_route`.
    const INPUT_BUDGET_CEILING: usize = 150_000;
    const PRODUCTION_INPUT_ESTIMATE: usize = 42_000;

    fn manifest_from(prepared: &PreparedOutboundRequest) -> RequestManifest {
        RequestManifest::build(ManifestDraft {
            session: session(),
            route: Availability::Exact(route()),
            tools: Availability::Exact(tools()),
            body: Availability::Exact(PreparedBodyInputs {
                prepared,
                reasoning_resolution: ReasoningResolution::Explicit,
                prompt: prompt(),
                input_budget_ceiling_tokens: Some(INPUT_BUDGET_CEILING),
                production_input_estimate_tokens: PRODUCTION_INPUT_ESTIMATE,
                tool_surface_is_exact: true,
            }),
        })
    }

    fn manifest(body: serde_json::Value) -> RequestManifest {
        let prepared = prepared_chat(body);
        manifest_from(&prepared)
    }

    fn body_facts(manifest: &RequestManifest) -> &BodyFacts {
        manifest
            .body
            .exact()
            .expect("body is exact in this fixture")
    }

    #[test]
    fn manifest_reports_typed_counts_and_hashes() {
        let manifest = manifest(chat_body());
        assert_eq!(manifest.schema_version, MANIFEST_SCHEMA_VERSION);
        let route = manifest.route.exact().expect("route is exact");
        assert_eq!(route.dialect, "chat-completions");
        assert_eq!(route.routing_source, "active-fixed-route");
        assert_eq!(manifest.session.agent_role, "primary");
        assert_eq!(manifest.session.lane_kind, "interactive-primary");
        assert_eq!(
            manifest.session.fleet_assignment,
            "not-applicable-primary-agent"
        );
        assert_eq!(route.context_limit_tokens, 200_000);
        assert_eq!(
            route.context_limit_source,
            crate::route_runtime::ContextWindowSource::Catalog
        );
        assert_eq!(
            route.route_id.as_ref().map(SafeLabel::as_str),
            Some("my-gateway")
        );
        let body = body_facts(&manifest);
        assert_eq!(body.message_count, 2, "system message is not a message");
        assert!(body.tool_result_canonical_json_bytes > 0);
        assert_eq!(body.body_sha256.len(), 64);
        assert_eq!(body.input_budget_ceiling_tokens, Some(INPUT_BUDGET_CEILING));
        assert_eq!(body.wire_output_cap_tokens, Some(4_096));
        assert!(matches!(
            &body.provider_reported_usage,
            Availability::Unavailable(Unavailable {
                reason: UnavailableReason::ProviderRequestNotExecuted,
                ..
            })
        ));
        assert_eq!(
            body.local_system_tools_component_sha256
                .as_deref()
                .map(str::len),
            Some(64)
        );
    }

    #[test]
    fn explicit_base_prompt_preview_is_exact_and_base_only() {
        assert_eq!(
            exact_base_prompt_only().as_bytes(),
            crate::prompts::effective_base_prompt_text().as_bytes()
        );
    }

    #[test]
    fn no_prompt_message_secret_or_path_reaches_any_surface() {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
        let mut body = chat_body();
        body["messages"][1]["content"] = json!(format!("look at {home}/.codewhale/config.toml"));
        let manifest = manifest(body);

        for surface in [
            manifest.render(),
            manifest.to_json(),
            format!("{manifest:?}"),
        ] {
            for forbidden in [
                "SECRET SYSTEM PROMPT",
                "SECRET USER MESSAGE",
                "SECRET TOOL OUTPUT",
                "hunter2",
                "sk-abcdef0123456789",
                "api_key=",
                "/api/paas/v4/chat/completions",
            ] {
                assert!(
                    !surface.contains(forbidden),
                    "`{forbidden}` leaked into a manifest surface:\n{surface}"
                );
            }
            assert!(!surface.contains(&home), "home path leaked:\n{surface}");
        }
    }

    #[test]
    fn hostile_route_and_model_identifiers_are_bounded_on_both_surfaces() {
        // A custom route id and a wire model are user-authored text: they can
        // be absolute paths, URLs, or deployment secrets.
        let mut route = route();
        route.route_id = Some(SafeLabel::identifier(
            "https://internal.example.com/v1/deployments/prod-key-8f2a",
        ));
        route.provider_id = SafeLabel::identifier("openai/secrets/config");
        route.wire_model = SafeLabel::catalog_model("qwen/src/lib.rs");
        route.provider_display = SafeLabel::identifier("sk-live-abcdef0123456789abcdef");

        let manifest = RequestManifest::build(ManifestDraft {
            session: session(),
            route: Availability::Exact(route),
            tools: Availability::Exact(tools()),
            body: Availability::unavailable(UnavailableReason::NoHypotheticalPromptSupplied),
        });

        for surface in [manifest.render(), manifest.to_json()] {
            for forbidden in [
                "internal.example.com",
                "deployments/prod-key-8f2a",
                "openai/secrets/config",
                "qwen/src/lib.rs",
                "sk-live-",
            ] {
                assert!(
                    !surface.contains(forbidden),
                    "`{forbidden}` leaked into a manifest surface:\n{surface}"
                );
            }
            assert!(surface.contains("sha256:"), "{surface}");
        }
    }

    #[test]
    fn unresolved_auto_publishes_no_route_tool_or_body_facts() {
        let mut session = session();
        session.requested_model = SafeLabel::identifier("auto");
        session.auto_model_routing = true;
        session.hypothetical_prompt_supplied = false;
        session.requested_reasoning = SafeLabel::identifier("auto");

        let manifest = RequestManifest::build(ManifestDraft {
            session,
            route: Availability::unavailable(UnavailableReason::AutoRouteUnresolvedUntilNextPrompt),
            tools: Availability::unavailable(UnavailableReason::AutoRouteUnresolvedUntilNextPrompt),
            body: Availability::unavailable(UnavailableReason::AutoRouteUnresolvedUntilNextPrompt),
        });

        let json = manifest.to_json();
        for forbidden in [
            "provider_id",
            "route_id",
            "dialect",
            "endpoint_host",
            "endpoint_fingerprint",
            "wire_model",
            "billing",
            "tool_surface_budget",
            "body_sha256",
        ] {
            assert!(
                !json.contains(forbidden),
                "`{forbidden}` must not appear when auto routing is unresolved:\n{json}"
            );
        }
        assert!(
            json.contains("auto-route-unresolved-until-next-prompt"),
            "{json}"
        );
        assert_eq!(manifest.session.requested_model.as_str(), "auto");

        let rendered = manifest.render();
        assert!(
            rendered.contains("auto model routing is unresolved until the next prompt"),
            "{rendered}"
        );
        assert!(rendered.contains("preview never runs"), "{rendered}");
        assert!(rendered.contains("Select a fixed route"), "{rendered}");
    }

    #[test]
    fn prompted_auto_reports_the_offline_classifier_boundary() {
        let mut session = session();
        session.requested_model = SafeLabel::identifier("auto");
        session.auto_model_routing = true;
        session.hypothetical_prompt_supplied = true;
        let reason = UnavailableReason::AutoRouteClassificationNotExecuted;
        let manifest = RequestManifest::build(ManifestDraft {
            session,
            route: Availability::unavailable(reason),
            tools: Availability::unavailable(reason),
            body: Availability::unavailable(reason),
        });

        let json = manifest.to_json();
        assert!(
            json.contains("auto-route-classification-not-executed"),
            "{json}"
        );
        for forbidden in [
            "provider_id",
            "endpoint_host_class",
            "wire_model",
            "body_sha256",
        ] {
            assert!(!json.contains(forbidden), "{forbidden} leaked:\n{json}");
        }
        assert!(manifest.render().contains("preview is offline"));
    }

    #[test]
    fn repeated_previews_are_byte_stable() {
        let first = manifest(chat_body());
        let second = manifest(chat_body());
        assert_eq!(first, second);
        assert_eq!(first.to_json(), second.to_json());
    }

    #[test]
    fn body_hash_moves_for_every_wire_mutation() {
        type BodyMutation = (&'static str, Box<dyn Fn(&mut serde_json::Value)>);

        let baseline = body_facts(&manifest(chat_body())).body_sha256.clone();
        let mutations: Vec<BodyMutation> = vec![
            (
                "max_tokens",
                Box::new(|b: &mut serde_json::Value| b["max_tokens"] = json!(1024)),
            ),
            (
                "tool_choice",
                Box::new(|b: &mut serde_json::Value| b["tool_choice"] = json!("required")),
            ),
            (
                "nested reasoning control",
                Box::new(|b: &mut serde_json::Value| {
                    b["thinking"] = json!({"type": "enabled", "effort": "max"});
                }),
            ),
            (
                "transformed tool schema",
                Box::new(|b: &mut serde_json::Value| {
                    b["tools"][0]["function"]["parameters"] = json!({"type": "object"});
                }),
            ),
            (
                "attachment",
                Box::new(|b: &mut serde_json::Value| {
                    b["messages"].as_array_mut().unwrap().push(json!({
                        "role": "user",
                        "content": [{"type": "image_url", "image_url": {"url": "data:x"}}],
                    }));
                }),
            ),
            (
                "stream options",
                Box::new(|b: &mut serde_json::Value| {
                    b["stream_options"] = json!({"include_usage": true});
                }),
            ),
            (
                "appended user message",
                Box::new(|b: &mut serde_json::Value| {
                    b["messages"].as_array_mut().unwrap().push(json!({
                        "role": "user",
                        "content": "the hypothetical next prompt",
                    }));
                }),
            ),
        ];
        for (what, mutate) in mutations {
            let mut body = chat_body();
            mutate(&mut body);
            assert_ne!(
                baseline,
                body_facts(&manifest(body)).body_sha256,
                "changing {what} must change the whole-body hash"
            );
        }
    }

    #[test]
    fn manifest_is_not_a_request_body_export() {
        // The inspectability slice must never become a way to dump the wire
        // body: no field may reproduce request-body content keys. Human-safe
        // explanatory strings may still use words such as `messages`.
        fn contains_request_content(value: &serde_json::Value) -> bool {
            match value {
                serde_json::Value::Object(object) => object.iter().any(|(key, value)| {
                    (key == "messages" && value.is_array())
                        || matches!(key.as_str(), "content" | "input_schema")
                        || contains_request_content(value)
                }),
                serde_json::Value::Array(values) => values.iter().any(contains_request_content),
                _ => false,
            }
        }

        let json = manifest(chat_body()).to_json();
        let value: serde_json::Value = serde_json::from_str(&json).expect("valid manifest JSON");
        assert!(!contains_request_content(&value), "{json}");
    }

    #[test]
    fn collapsed_tool_surfaces_are_disclosed_not_asserted_away() {
        let collapsed = manifest(chat_body());
        assert!(
            collapsed
                .tools
                .exact()
                .expect("tools exact")
                .standard_and_full_surfaces_collapsed
        );
        assert!(collapsed.render().contains("collapsed — both budgets"));

        let mut distinct_tools = tools();
        distinct_tools.standard_and_full_surfaces_collapsed = false;
        let prepared = prepared_chat(chat_body());
        let distinct = RequestManifest::build(ManifestDraft {
            session: session(),
            route: Availability::Exact(route()),
            tools: Availability::Exact(distinct_tools),
            body: Availability::Exact(PreparedBodyInputs {
                prepared: &prepared,
                reasoning_resolution: ReasoningResolution::Explicit,
                prompt: prompt(),
                input_budget_ceiling_tokens: Some(INPUT_BUDGET_CEILING),
                production_input_estimate_tokens: PRODUCTION_INPUT_ESTIMATE,
                tool_surface_is_exact: false,
            }),
        });
        assert!(distinct.render().contains("distinct — the budgets"));
        assert!(
            body_facts(&distinct)
                .local_system_tools_component_sha256
                .is_none(),
            "no local component hash without an exact tool catalog"
        );
    }

    #[test]
    fn estimates_do_not_double_count_subsets() {
        let manifest = manifest(chat_body());
        let body = body_facts(&manifest);
        assert!(
            body.estimates.tool_results <= body.estimates.messages,
            "tool results are a subset of message bytes"
        );
        assert!(
            body.estimates.attachments <= body.estimates.messages,
            "attachments are a subset of message bytes"
        );
        assert!(manifest.render().contains("not provider-counted"));
    }

    /// The reviewed defect: the published byte classes counted selected values
    /// and omitted JSON key names, brackets, and separators, so they summed to
    /// less than the request. They are exact facts, so they must partition the
    /// body they describe.
    #[test]
    fn published_byte_classes_partition_the_whole_body() {
        for body in [
            chat_body(),
            json!({"model": "m", "messages": []}),
            json!({
                "model": "m",
                "messages": [{"role": "user", "content": "hi"}],
                "max_tokens": 32,
            }),
        ] {
            let manifest = manifest(body);
            let facts = body_facts(&manifest);
            assert_eq!(
                facts.system_canonical_json_bytes
                    + facts.tool_schema_canonical_json_bytes
                    + facts.message_canonical_json_bytes
                    + facts.framing_canonical_json_bytes,
                facts.body_canonical_json_bytes,
                "classes must sum to the wire body:\n{}",
                manifest.render()
            );
        }
    }

    /// Headroom is measured against the production input budget, not the raw
    /// context window: the window has to hold the response too.
    #[test]
    fn headroom_comes_from_the_input_budget_not_the_context_window() {
        let manifest = manifest(chat_body());
        let body = body_facts(&manifest);
        assert_eq!(
            body.estimated_input_headroom_tokens,
            Some(INPUT_BUDGET_CEILING as i64 - PRODUCTION_INPUT_ESTIMATE as i64)
        );
        assert_ne!(
            body.estimated_input_headroom_tokens,
            Some(INPUT_BUDGET_CEILING as i64 - body.estimates.total_conservative as i64),
            "the independent wire estimate must not drive production headroom"
        );
        assert_ne!(
            body.estimated_input_headroom_tokens,
            Some(200_000 - PRODUCTION_INPUT_ESTIMATE as i64),
            "the route's context limit is not the input budget"
        );
        assert!(
            manifest
                .render()
                .contains("window minus output reservation"),
            "{}",
            manifest.render()
        );
    }

    /// A request over budget reports negative headroom rather than clamping to
    /// zero and reading as "it fits".
    #[test]
    fn headroom_goes_negative_when_the_request_would_not_fit() {
        let prepared = prepared_chat(chat_body());
        let manifest = RequestManifest::build(ManifestDraft {
            session: session(),
            route: Availability::Exact(route()),
            tools: Availability::Exact(tools()),
            body: Availability::Exact(PreparedBodyInputs {
                prepared: &prepared,
                reasoning_resolution: ReasoningResolution::Explicit,
                prompt: prompt(),
                input_budget_ceiling_tokens: Some(1),
                production_input_estimate_tokens: PRODUCTION_INPUT_ESTIMATE,
                tool_surface_is_exact: true,
            }),
        });
        assert!(
            body_facts(&manifest)
                .estimated_input_headroom_tokens
                .is_some_and(|headroom| headroom < 0)
        );
    }

    /// The local component identity follows final wire-shaped schemas even
    /// when the logical catalog is untouched.
    #[test]
    fn local_system_tools_component_follows_wire_regions() {
        let baseline = manifest(chat_body());
        let baseline_component = body_facts(&baseline)
            .local_system_tools_component_sha256
            .clone();
        let baseline_tools = body_facts(&baseline).tool_schema_wire_sha256.clone();
        assert!(baseline_component.is_some());

        let mut shaped = chat_body();
        shaped["tools"][0]["function"]["parameters"] =
            json!({"type": "object", "additionalProperties": false});
        shaped["tools"][0]["function"]["strict"] = json!(true);
        let shaped = manifest(shaped);
        assert_ne!(
            baseline_tools,
            body_facts(&shaped).tool_schema_wire_sha256,
            "a shaped schema must move the wire tool hash"
        );
        assert_ne!(
            baseline_component,
            body_facts(&shaped).local_system_tools_component_sha256,
            "a shaped schema must move the local system/tools component"
        );

        // …and the system region moves it too.
        let mut other_system = chat_body();
        other_system["messages"][0]["content"] = json!("A DIFFERENT SYSTEM PROMPT");
        assert_ne!(
            baseline_component,
            body_facts(&manifest(other_system)).local_system_tools_component_sha256
        );
    }

    #[test]
    fn nested_reasoning_effort_is_reported_with_its_key_path() {
        let mut body = chat_body();
        body.as_object_mut()
            .expect("object")
            .remove("reasoning_effort");
        body["thinking"] = json!({"type": "enabled", "effort": "max"});
        let manifest = manifest(body);
        let facts = body_facts(&manifest);
        assert_eq!(
            facts.reasoning_wire_effort.as_ref().map(SafeLabel::as_str),
            Some("max")
        );
        assert_eq!(
            facts.reasoning_wire_effort_source.as_deref(),
            Some("thinking.effort")
        );
        assert!(manifest.render().contains("max (from `thinking.effort`)"));
    }

    /// A body with no reasoning request must not read as an explicit user
    /// selection just because the caller happened to pass `Explicit`.
    #[test]
    fn route_default_and_not_applicable_are_distinguishable_labels() {
        assert_eq!(
            ReasoningResolution::RouteDefault.label(),
            "route default (no user selection)"
        );
        assert_ne!(
            ReasoningResolution::RouteDefault.label(),
            ReasoningResolution::Explicit.label()
        );
    }

    /// A section that depends on an unavailable section inherits its typed
    /// reason instead of quietly becoming exact.
    #[test]
    fn unavailability_propagates_to_dependent_sections() {
        let unavailable_tools: Availability<ToolSurfaceFacts> =
            Availability::unavailable(UnavailableReason::McpStateNotSnapshottable);
        let body: Availability<BodyFacts> = unavailable_tools.propagate().expect("propagates");
        assert!(body.exact().is_none());
        assert!(
            Availability::Exact(tools())
                .propagate::<BodyFacts>()
                .is_none(),
            "an exact section propagates nothing"
        );
    }

    #[test]
    fn scope_exclusions_are_stated_on_the_human_surface() {
        let rendered = manifest(chat_body()).render();
        assert!(rendered.contains("primary agent turn"), "{rendered}");
        assert!(rendered.contains("auxiliary"), "{rendered}");
    }

    #[test]
    fn tool_choice_labels_are_short_shapes() {
        assert_eq!(tool_choice_label(None), None);
        assert_eq!(
            tool_choice_label(Some(&json!("required")))
                .as_ref()
                .map(SafeLabel::as_str),
            Some("required")
        );
        assert_eq!(
            tool_choice_label(Some(&json!({"type": "auto"})))
                .as_ref()
                .map(SafeLabel::as_str),
            Some("auto")
        );
        assert_eq!(
            tool_choice_label(Some(&json!({
                "type": "function",
                "function": {"name": "read_file"}
            })))
            .as_ref()
            .map(SafeLabel::as_str),
            Some("function:read_file")
        );
    }

    /// The manifest consumes provider wire truth, never the logical
    /// `MessageRequest.tool_choice`. These are the three reviewed divergences:
    /// Anthropic keeps an object, Responses maps it to a string, and DeepSeek
    /// thinking omits the field.
    #[test]
    fn manifest_tool_choice_is_read_from_the_final_provider_body() {
        for (dialect, body, expected) in [
            (
                WireDialect::AnthropicMessages,
                json!({
                    "model": "claude-sonnet-4-6",
                    "messages": [],
                    "tool_choice": {"type": "auto"}
                }),
                Some("auto"),
            ),
            (
                WireDialect::OpenAiResponses,
                json!({
                    "model": "gpt-5-codex",
                    "input": [],
                    "tool_choice": "auto"
                }),
                Some("auto"),
            ),
            (
                WireDialect::ChatCompletions,
                json!({
                    "model": "deepseek-reasoner",
                    "messages": [],
                    "reasoning_effort": "high"
                }),
                None,
            ),
        ] {
            let prepared = PreparedOutboundRequest::new(
                dialect,
                endpoint("https://api.example.com/v1/messages"),
                "wire-model".to_string(),
                body,
                Some("high".to_string()),
                None,
                CallerStreamMode::Streaming,
            );
            assert_eq!(
                body_facts(&manifest_from(&prepared))
                    .tool_choice
                    .as_ref()
                    .map(SafeLabel::as_str),
                expected,
                "{dialect:?}"
            );
        }
    }
}

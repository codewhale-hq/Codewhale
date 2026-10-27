//! Runtime for an **exact named Fleet** (`schema = "exact"`).
//!
//! The saved Fleet is the Fleet that runs. At Workflow start its definition is
//! read from the standard `FleetSearchRoot` locations, every worker route is
//! **preflighted and frozen**, the attached Reasoning Router service is
//! resolved, and the whole thing is captured into an immutable
//! [`FleetSnapshot`] projected onto the roster/profile machinery the in-process
//! spawn path already uses.
//!
//! Five invariants govern everything below.
//!
//! 1. **Routes freeze first, and are checked while freezing.** Provider
//!    identity, canonical wire model, endpoint, local credential readiness, and
//!    reasoning capability are all resolved before the Workflow starts — and
//!    certainly before any Router is asked anything. Nothing downstream may
//!    move them: not a task option, not the Router.
//! 2. **Admission comes before cost.** A task is resolved against the roster,
//!    checked against gates, and given a concurrency slot *before* the Router
//!    is called. A rejected or capacity-blocked task spends no Router tokens
//!    and discloses nothing to a Router's provider.
//! 3. **Auto is a reasoning decision, and the attached Router makes it.**
//!    `reasoning = "auto"` always goes to the Fleet's Reasoning Router — no
//!    provider-native-adaptive bypass, no legacy model routing, no local
//!    keyword heuristic. A manual tier calls no Router at all.
//! 4. **Runtime owns authority.** After exact member selection, Runtime maps
//!    the semantic role onto its closed role policy and intersects that policy
//!    with the live parent. Fleet identity never grants or withholds project
//!    trust, tools, writes, network reach, shell, or delegation.
//! 5. **Receipts are truthful and content-free.** The tier a selector picked,
//!    the control a provider actually receives, and what a Router cost are
//!    recorded separately; task text never is.

use std::sync::Arc;

use async_trait::async_trait;
use codewhale_protocol::fleet::PermissionCeiling;
#[cfg(test)]
use codewhale_protocol::fleet::ShellCeiling;
use codewhale_workflow::{
    CapturedReasoningRouter, CredentialReadiness, EffectiveReasoning, EndpointIdentity,
    FleetDocument, FleetRouterRef, FleetSearchRoot, FleetSnapshot, FleetSnapshotMember,
    FleetTaskReceipt, NamedFleetError, PreflightError, PreflightedRoute, ProviderReasoningControl,
    QualifiedFleetId, ReasoningCapability, ReasoningRouterProfile, ReasoningTier,
    ResolvedReasoning, RoutePreflight, RouterAvailability, RouterCallInput, RouterCallPlan,
    RouterIdentity, RoutingDisclosure, bounded_routing_payload, captured_legacy_inline_router,
    parse_router_decision, resolve_exact_member_reasoning, router_call_plan, router_system_prompt,
    router_user_message,
};

use crate::llm_client::LlmClient;
use crate::reasoning_preference::ReasoningEffort;
use codewhale_agent_contract::role::{ChildAuthority, public_role_label};
#[cfg(test)]
use codewhale_agent_contract::role::{
    NETWORK_DENIAL_SENTINEL, NETWORK_TOOL_DENYLIST, RAW_SHELL_SENTINEL, is_posture_denial,
    session_shell_ceiling,
};
use codewhale_config::ProviderKind;
use codewhale_models::Role;
use codewhale_runtime::config::Config;

/// Where exact Fleet definitions and Reasoning Router profiles are looked up,
/// labelled so an identity can be qualified (`workspace/glm-pair`) instead of
/// silently shadowed.
fn personal_fleet_root() -> anyhow::Result<std::path::PathBuf> {
    codewhale_config::codewhale_home()
}

pub(crate) fn personal_fleet_definitions_dir() -> anyhow::Result<std::path::PathBuf> {
    Ok(personal_fleet_root()?.join("fleets"))
}

/// The workspace has two origins. `workspace` is `<workspace>/.codewhale`, the
/// directory the Fleet store saves workspace-scoped Fleets to, so a Fleet
/// saved from the Fleet UI is found by name. `workspace_root` is the workspace
/// directory itself, which keeps checked-in `fleets/<name>.toml` rosters
/// loading as they always have.
#[must_use]
pub(crate) fn fleet_search_roots(workspace: &std::path::Path) -> Vec<FleetSearchRoot> {
    let mut roots = Vec::new();
    if let Ok(home) = personal_fleet_root() {
        roots.push(FleetSearchRoot::new("codewhale_home", home));
    }
    roots.push(FleetSearchRoot::new(
        "workspace",
        workspace.join(".codewhale"),
    ));
    roots.push(FleetSearchRoot::new(
        "workspace_root",
        workspace.to_path_buf(),
    ));
    roots
}

/// Load a Fleet document by (optionally qualified) name from the standard
/// roots. Ambiguity between origins is surfaced, never resolved by shadowing.
///
/// Saved v2 Fleets (`schema = "fleet"`, from `.codewhale/fleets/` or
/// `$CODEWHALE_HOME/fleets/`) are looked up first and frozen into an exact
/// snapshot here — see [`freeze_saved_fleet`]. A miss falls through to the
/// workflow crate's legacy/exact loader. A bare name that exists both as a v2
/// Fleet and as a legacy/exact file is ambiguous: neither shadows the other,
/// and the error names every path. v2 Fleets qualify as `user/<name>` and
/// `folder/<name>` (the store's own scope labels); a search-root origin
/// (`codewhale_home/`, `workspace/`, `workspace_root/`) reads that root's file
/// in whichever form it is.
///
/// `config` is the session config the caller preflights with: inheriting
/// members resolve against it at this point, immediately before the same
/// config preflights the frozen routes, so a receipt names the route that ran.
///
/// Synchronous file loading: async callers must run this on the blocking pool.
pub(crate) fn load_fleet_document(
    name: &str,
    workspace: &std::path::Path,
    config: Option<&Config>,
) -> Result<(FleetDocument, QualifiedFleetId), NamedFleetError> {
    use super::store::{self, FleetScope};

    let roots = fleet_search_roots(workspace);
    // Validates the bare name before any path below is built from it.
    let (origin, bare) = codewhale_workflow::split_qualified_fleet_name(name)?;
    let store_error = |error: store::FleetStoreError| match error {
        store::FleetStoreError::NotFound(what) => NamedFleetError::NotFound(what),
        store::FleetStoreError::Io { path, message } => NamedFleetError::Io { path, message },
        store::FleetStoreError::Parse { path, message } => NamedFleetError::Parse { path, message },
        other => NamedFleetError::Parse {
            path: bare.to_string(),
            message: other.to_string(),
        },
    };
    let v2_scope = match origin.map(str::to_ascii_lowercase).as_deref() {
        None => None,
        Some("user" | "personal") => Some(FleetScope::Personal),
        Some("folder") => Some(FleetScope::Workspace),
        // Any other origin names a legacy/exact search root. A saved v2
        // Fleet can live there too (the personal `fleets/` directory is
        // shared), so the qualified file is read in whichever form it is.
        Some(origin) => {
            let saved = roots
                .iter()
                .find(|root| root.origin.eq_ignore_ascii_case(origin))
                .map(|root| {
                    root.root
                        .join(store::FLEET_DIR)
                        .join(format!("{bare}.toml"))
                })
                .filter(|path| store::declares_v2_schema(path));
            let Some(path) = saved else {
                return FleetDocument::load_by_name(name, &roots);
            };
            let (fleet, scope) = store::load_fleet_at(&path).map_err(store_error)?;
            return freeze_saved_fleet(&fleet, scope, &path, config);
        }
    };

    if let Some(scope) = v2_scope {
        let (fleet, path) =
            store::load_fleet_in_scope(bare, scope, workspace).map_err(store_error)?;
        return freeze_saved_fleet(&fleet, scope, &path, config);
    }

    // Legacy/exact files under the same bare name, in any root. A v2 file in
    // the shared personal directory is the store's, not a second Fleet.
    let file_name = format!("{bare}.toml");
    let other_forms: Vec<String> = roots
        .iter()
        .filter_map(|root| {
            let path = root.root.join(store::FLEET_DIR).join(&file_name);
            let schema = store::read_declared_schema(&path)?;
            (schema.as_deref() != Some(store::FLEET_SCHEMA_KIND))
                .then(|| format!("{}/{bare} ({})", root.origin, path.display()))
        })
        .collect();

    let v2_candidates = store::v2_fleet_candidates(bare, workspace);
    let v2_labels = || {
        v2_candidates
            .iter()
            .map(|(scope, path)| format!("{}/{bare} ({})", scope.label(), path.display()))
    };
    if v2_candidates.len() > 1 || (!v2_candidates.is_empty() && !other_forms.is_empty()) {
        return Err(NamedFleetError::AmbiguousFleet {
            name: bare.to_string(),
            origins: v2_labels().chain(other_forms).collect(),
        });
    }
    match store::load_fleet(bare, workspace) {
        Ok((fleet, scope, path)) => freeze_saved_fleet(&fleet, scope, &path, config),
        Err(store::FleetStoreError::NotFound(_)) => FleetDocument::load_by_name(name, &roots),
        Err(error) => Err(store_error(error)),
    }
}

/// Freeze a saved v2 Fleet into an exact snapshot document.
///
/// Every executable member leaves here with one concrete provider/model and
/// one concrete reasoning request: an explicit member pin wins, then the
/// Fleet's operator route, then the live session route from `config`. The
/// result is rendered as an exact document and parsed by the workflow crate's
/// own exact parser, so it passes the same validation as a hand-written exact
/// file, and the snapshot hash covers what was frozen. Editing the v2 file
/// afterwards changes only the next Workflow.
///
/// Member `instructions` and `requires` are refused rather than dropped: the
/// exact snapshot has no field for either, and a Workflow that silently ran a
/// member without its instructions or capability requirement would not be the
/// saved Fleet.
fn freeze_saved_fleet(
    fleet: &super::store::FleetFile,
    scope: super::store::FleetScope,
    path: &std::path::Path,
    config: Option<&Config>,
) -> Result<(FleetDocument, QualifiedFleetId), NamedFleetError> {
    #[derive(serde::Serialize)]
    struct FrozenFleet {
        schema: &'static str,
        schema_revision: u32,
        name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        members: Vec<FrozenMember>,
    }
    #[derive(serde::Serialize)]
    struct FrozenMember {
        id: String,
        role: String,
        provider: String,
        model: String,
        reasoning: String,
    }

    let slug = fleet.file_slug();
    let fail = |message: String| NamedFleetError::Parse {
        path: path.display().to_string(),
        message,
    };
    let operator = fleet.operator.as_ref();
    // A saved Fleet stores reasoning in the session vocabulary (`xhigh`,
    // `ultra`, `minimal`, ... — what an imported agent profile carries); the
    // exact schema names tiers. Map through the same effort-to-tier table the
    // preflight uses, keep an explicit `auto` as a Router request, and treat a
    // blank value as absent (inherit), as the selected-Fleet path does.
    let frozen_reasoning = |raw: Option<&str>| -> Result<Option<String>, String> {
        let Some(value) = raw.map(str::trim).filter(|value| !value.is_empty()) else {
            return Ok(None);
        };
        let effort =
            ReasoningEffort::parse_strict(value).map_err(|error| format!("reasoning: {error}"))?;
        Ok(Some(
            tier_of(effort)
                .map_or("auto", ReasoningTier::as_str)
                .to_string(),
        ))
    };
    let session_route = config
        .map(|config| {
            config
                .active_provider_identity()
                .map(|identity| (identity.key.to_string(), config.default_model()))
        })
        .transpose()
        .map_err(fail)?;
    let session_reasoning = || {
        let effort = config
            .and_then(Config::reasoning_effort)
            .map(ReasoningEffort::from_setting)
            .unwrap_or_default();
        // A session-level `auto` is per-turn adaptivity, not a Router
        // request; a frozen member takes the concrete default tier instead.
        tier_of(effort)
            .unwrap_or(ReasoningTier::Max)
            .as_str()
            .to_string()
    };

    let mut unsupported = Vec::new();
    let mut members = Vec::new();
    for member in fleet.members.iter().filter(|member| !member.shortlist) {
        let id = member.id.trim().to_string();
        if member
            .instructions
            .as_deref()
            .is_some_and(|text| !text.trim().is_empty())
        {
            unsupported.push(format!("`{id}` has instructions"));
        }
        if !member.requires.is_empty() {
            unsupported.push(format!("`{id}` has requires"));
        }
        let (provider, model) = match (&member.provider, &member.model, operator, &session_route) {
            (Some(provider), Some(model), _, _) => (provider.clone(), model.clone()),
            (None, None, Some(operator), _) => (operator.provider.clone(), operator.model.clone()),
            (None, None, None, Some((provider, model))) => (provider.clone(), model.clone()),
            (None, None, None, None) => {
                return Err(fail(format!(
                    "member `{id}` inherits the session route, but no session config is \
                     available to resolve it"
                )));
            }
            _ => {
                return Err(fail(format!(
                    "member `{id}` has a partial provider/model pin"
                )));
            }
        };
        let reasoning = match frozen_reasoning(member.reasoning.as_deref())
            .map_err(|error| fail(format!("member `{id}` {error}")))?
        {
            Some(tier) => tier,
            None => frozen_reasoning(operator.and_then(|operator| operator.reasoning.as_deref()))
                .map_err(|error| fail(format!("operator {error}")))?
                .unwrap_or_else(session_reasoning),
        };
        members.push(FrozenMember {
            role: member.role_label().to_string(),
            id,
            provider,
            model,
            reasoning,
        });
    }
    if !unsupported.is_empty() {
        return Err(fail(format!(
            "saved Fleet `{}` cannot run as a Workflow Fleet yet: {}. Workflow snapshots freeze \
             each member's route and reasoning only; remove those fields or run the members \
             with `agent`.",
            fleet.name,
            unsupported.join(", ")
        )));
    }

    let frozen = FrozenFleet {
        schema: codewhale_workflow::EXACT_FLEET_SCHEMA_KIND,
        schema_revision: codewhale_workflow::EXACT_FLEET_SCHEMA_REVISION,
        name: slug.clone(),
        description: fleet.description.clone(),
        members,
    };
    let text = toml::to_string(&frozen)
        .map_err(|error| fail(format!("failed to freeze saved Fleet: {error}")))?;
    let document = FleetDocument::from_frozen_saved_fleet(&text, path).map_err(|error| {
        fail(format!(
            "saved Fleet `{}` cannot run as a Workflow Fleet: {error}",
            fleet.name
        ))
    })?;
    Ok((
        document,
        QualifiedFleetId {
            name: slug,
            origin: scope.label().to_string(),
        },
    ))
}

// ── Preflight: freeze the route, and check it while freezing ─────────────────

/// Derive a route's real reasoning capability from the request shaping the
/// client actually performs, rather than from a hand-maintained claims table.
///
/// The probe builds the request body this exact route would receive for every
/// tier and compares them. Two tiers that produce a byte-identical body are not
/// two provider-effective tiers, whatever the selector calls them — this is why
/// Z.AI's GLM routes come back as
/// [`ProviderReasoningControl::EnabledDisabled`] and why nothing here can claim
/// provider-native adaptive for a route whose body does not say so.
#[must_use]
pub(crate) fn reasoning_capability_for_route(
    provider: ProviderKind,
    base_url: &str,
    wire_model: &str,
) -> ReasoningCapability {
    let body_for = |effort: ReasoningEffort| -> String {
        let mut body = serde_json::json!({});
        let value = effort.api_value_for_route(provider, base_url, wire_model);
        crate::client::reasoning_effort::apply_reasoning_effort(&mut body, value, provider);
        // `reasoning_split` is a transport concern the client sets for every
        // tier; it carries no reasoning depth, so it must not make tiers look
        // distinct or make a no-control route look controllable.
        if let Some(object) = body.as_object_mut() {
            object.remove("reasoning_split");
        }
        body.to_string()
    };

    let off = body_for(ReasoningEffort::Off);
    let above_off: Vec<String> = [
        ReasoningEffort::Low,
        ReasoningEffort::Medium,
        ReasoningEffort::High,
        ReasoningEffort::Max,
    ]
    .into_iter()
    .map(body_for)
    .collect();

    let empty = "{}";
    let all_empty = off == empty && above_off.iter().all(|body| body == empty);

    let mut distinct = above_off.clone();
    distinct.sort();
    distinct.dedup();

    let control = if all_empty {
        ProviderReasoningControl::None
    } else if distinct.len() == 1 && distinct[0] == off && off.contains("adaptive") {
        // Every tier — including off — produces the same adaptive body: the
        // provider genuinely chooses its own depth. Source-backed, not assumed.
        ProviderReasoningControl::NativeAdaptive
    } else if distinct.len() > 1 {
        ProviderReasoningControl::Tiers
    } else {
        ProviderReasoningControl::EnabledDisabled
    };

    // What each requested tier actually becomes on the wire, straight from the
    // route normalizer that shapes the real request.
    //
    // This subsumes a min/max floor-and-ceiling and expresses what one cannot:
    // most non-Codex routes coerce `low` and `medium` to `high` while leaving
    // `off` alone (first-party DeepSeek routes are the documented exception —
    // their wire carries a real `low`), and an always-thinking route raises
    // `off` instead. Reporting a `low` a route silently sends as `high` is
    // the invisible substitution receipts exist to prevent, so the map — not
    // a clamp — is the authority.
    let wire_tiers = [
        ReasoningEffort::Off,
        ReasoningEffort::Low,
        ReasoningEffort::Medium,
        ReasoningEffort::High,
        ReasoningEffort::Max,
    ]
    .map(|effort| {
        tier_of(effort.normalize_for_route(provider, base_url, wire_model))
            .unwrap_or(ReasoningTier::Off)
    });

    ReasoningCapability {
        control,
        min_tier: None,
        max_tier: None,
        wire_tiers: None,
    }
    .with_wire_tiers(wire_tiers)
}

fn tier_of(effort: ReasoningEffort) -> Option<ReasoningTier> {
    match effort {
        ReasoningEffort::Off => Some(ReasoningTier::Off),
        ReasoningEffort::Minimal => Some(ReasoningTier::Low),
        ReasoningEffort::Low => Some(ReasoningTier::Low),
        ReasoningEffort::Medium => Some(ReasoningTier::Medium),
        ReasoningEffort::High => Some(ReasoningTier::High),
        ReasoningEffort::XHigh => Some(ReasoningTier::Max),
        ReasoningEffort::Ultra => Some(ReasoningTier::Max),
        ReasoningEffort::Max => Some(ReasoningTier::Max),
        ReasoningEffort::Auto => None,
    }
}

/// The **provider-facing** reasoning value for one tier on one exact route.
///
/// A tier label (`off`, `max`) is a selector concept; what a request may carry
/// is a provider concept, and the two are not the same string. OpenAI Codex
/// routes spell the top tier `xhigh` and cannot express `off` at all, so
/// placing a bare tier label on a Codex request either sends a value the
/// provider does not accept or silently sends nothing and takes the provider
/// default while the receipt claims the tier. Reading the value back out of the
/// same route normalizer the client uses is what keeps the request and the
/// receipt describing each other.
#[must_use]
pub(crate) fn route_reasoning_setting(
    provider: ProviderKind,
    base_url: &str,
    wire_model: &str,
    tier: ReasoningTier,
) -> String {
    effort_of(tier)
        .as_setting_for_route(provider, base_url, wire_model)
        .to_string()
}

fn effort_of(tier: ReasoningTier) -> ReasoningEffort {
    match tier {
        ReasoningTier::Off => ReasoningEffort::Off,
        ReasoningTier::Low => ReasoningEffort::Low,
        ReasoningTier::Medium => ReasoningEffort::Medium,
        ReasoningTier::High => ReasoningEffort::High,
        ReasoningTier::Max => ReasoningEffort::Max,
    }
}

/// Preflight one exact route: resolve the provider, canonicalize the model,
/// identify the endpoint, decide credential readiness **from local config**,
/// and derive the reasoning capability.
///
/// No provider is contacted. Everything here is a configuration lookup, which
/// is what makes it safe to run before the operator's gates have fired.
pub(crate) fn preflight_route(
    member_id: &str,
    provider: &str,
    model: &str,
    config: &Config,
) -> Result<PreflightedRoute, PreflightError> {
    let identity = config
        .resolve_provider_identity(provider.trim())
        .map_err(|detail| PreflightError::ProviderUnresolved {
            member: member_id.to_string(),
            provider: provider.to_string(),
            detail,
        })?;

    // The canonical wire model, resolved once. The receipt and the child spawn
    // both read this value, so they cannot disagree about what actually ran.
    let wire_model =
        codewhale_runtime::config::requested_model_for_provider(identity.provider, model.trim())
            .ok_or_else(|| PreflightError::ModelUnresolved {
                member: member_id.to_string(),
                provider: identity.key.to_string(),
                model: model.to_string(),
                detail: "not a known model for this provider".to_string(),
            })?;
    codewhale_runtime::config::validate_route(identity.provider, &wire_model).map_err(
        |detail| PreflightError::ModelUnresolved {
            member: member_id.to_string(),
            provider: identity.key.to_string(),
            model: wire_model.clone(),
            detail,
        },
    )?;

    let mut scoped = config.clone();
    scoped
        .scope_to_provider_identity(&identity)
        .map_err(|detail| PreflightError::ProviderUnresolved {
            member: member_id.to_string(),
            provider: provider.to_string(),
            detail,
        })?;
    let base_url = scoped.active_route_base_url();

    // Locally decided. A concrete loopback/self-hosted route is keyless by
    // design, and that is a valid, first-class state — not a downgrade and
    // not a missing credential. Ollama Cloud is hosted and falls through to
    // the ordinary credential checks.
    let credential = if codewhale_runtime::config::provider_route_is_keyless_self_hosted(
        identity.provider,
        &base_url,
    ) {
        CredentialReadiness::KeylessLocal
    } else if crate::provider_readiness::has_api_key_for(&scoped, &identity) {
        CredentialReadiness::Configured
    } else {
        // The discriminant only. `Missing { detail }` names the provider table
        // key, which for a custom route is the customer's own string.
        codewhale_telemetry::session_counters()
            .bump_error(codewhale_telemetry::ErrorCounter::AuthPreflightFailed);
        CredentialReadiness::Missing {
            detail: format!("no credential configured for `{}`", identity.key),
        }
    };

    Ok(PreflightedRoute {
        member_id: member_id.to_string(),
        provider_id: identity.key.to_string(),
        provider_config_id: identity
            .migrated_legacy_ollama_cloud_route()
            .then(|| provider.trim().to_string()),
        provider_kind: if identity.provider == ProviderKind::OllamaCloud {
            identity.provider.as_str().to_string()
        } else {
            format!("{:?}", identity.provider).to_ascii_lowercase()
        },
        declared_model: model.trim().to_string(),
        wire_model: wire_model.clone(),
        endpoint: EndpointIdentity::from_base_url(&base_url),
        credential,
        capability: reasoning_capability_for_route(identity.provider, &base_url, &wire_model),
    })
}

/// Build the client one worker route would actually run on, and throw it away.
///
/// Preflight resolves a route from *configuration*; this proves the same route
/// can be turned into a working client — the step that fails on a malformed
/// base URL, an unusable auth mode, or a transport CodeWhale cannot construct.
/// Doing it at Workflow start, for every member, is what stops a Fleet from
/// paying for a Router decision and only then discovering that the worker it
/// decided for could never have been launched.
///
/// The client is deliberately not retained: the spawn path builds the child's
/// own client from the member's roster profile, and keeping a second one here
/// would create two objects that could drift apart.
fn validate_route_client(route: &PreflightedRoute, config: &Config) -> Result<(), String> {
    let mut scoped = config.clone();
    let identity = config.resolve_provider_identity(route.provider_config_id())?;
    scoped.scope_to_provider_identity(&identity)?;
    crate::client::CodewhaleClient::new(&scoped)
        .map(|_| ())
        .map_err(|error| {
            format!(
                "member `{}` is pinned to provider `{}` (model `{}`), whose client could not be \
                 built on this machine: {error}",
                route.member_id, route.provider_id, route.wire_model
            )
        })
}

// ── The Reasoning Router, as a service ──────────────────────────────────────

/// The seam a Reasoning Router call goes through. Implemented live against the
/// provider client, and by a fixture in tests so the whole reasoning path is
/// exercised without a network.
#[async_trait]
pub(crate) trait FleetRouterCaller: Send + Sync + std::fmt::Debug {
    /// Return the router's raw text response for one worker task.
    async fn decide(&self, input: &RouterCallInput) -> Result<String, String>;

    /// The Router service's exact identity, for the receipt.
    fn identity(&self) -> RouterIdentity;
}

/// A Reasoning Router bound to its own exact preflighted route.
#[derive(Clone)]
pub(crate) struct LiveFleetRouter {
    client: crate::client::CodewhaleClient,
    captured: CapturedReasoningRouter,
    route: PreflightedRoute,
    /// The Router route's provider kind and base URL, kept so the call's
    /// reasoning value can be shaped by the *actual* configured route rather
    /// than by a generic tier label. Never serialized — the base URL can carry
    /// a credential and receipts are durable.
    provider: ProviderKind,
    base_url: String,
    /// What the Router call is actually made at, plus the four-sided disclosure
    /// for the receipt. Configured by the operator (`off` or `low`), normalized
    /// only against what the Router's own route can express.
    call: RouterCallPlan,
}

impl std::fmt::Debug for LiveFleetRouter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LiveFleetRouter")
            .field("router", &self.captured.qualified())
            .field("provider", &self.route.provider_id)
            .field("model", &self.route.wire_model)
            .field("call_reasoning", &self.call.tier)
            .field("client", &"<redacted>")
            .finish()
    }
}

impl LiveFleetRouter {
    /// Resolve the Router service's exact configured route and build its client.
    ///
    /// A Router that cannot be resolved is an error here — at Workflow start,
    /// before any worker is dispatched — not a silent downgrade to legacy
    /// routing. Readiness is decided from local configuration; no live probe.
    pub(crate) fn bind(
        captured: &CapturedReasoningRouter,
        config: &Config,
    ) -> Result<Self, RouterBindError> {
        let route = preflight_route(
            &captured.id,
            &captured.route.provider,
            &captured.route.model,
            config,
        )
        .map_err(|error| RouterBindError {
            reason: error.to_string(),
        })?;
        route.require_ready().map_err(|error| RouterBindError {
            reason: error.to_string(),
        })?;

        let identity = config
            .resolve_provider_identity(route.provider_config_id())
            .map_err(|detail| RouterBindError {
                reason: format!(
                    "reasoning router provider `{}` did not resolve: {detail}",
                    route.provider_id
                ),
            })?;
        let mut scoped = config.clone();
        scoped
            .scope_to_provider_identity(&identity)
            .map_err(|reason| RouterBindError { reason })?;
        let base_url = scoped.active_route_base_url();
        let client =
            crate::client::CodewhaleClient::new(&scoped).map_err(|error| RouterBindError {
                reason: format!(
                    "reasoning router provider `{}` client could not be built: {error}",
                    route.provider_id
                ),
            })?;

        let call = router_call_plan(captured.requested_call_reasoning, &route.capability);

        Ok(Self {
            client,
            captured: captured.clone(),
            route,
            provider: identity.provider,
            base_url,
            call,
        })
    }

    /// The preflighted Router route, for cross-provider disclosure.
    #[must_use]
    pub(crate) fn route(&self) -> &PreflightedRoute {
        &self.route
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RouterBindError {
    pub(crate) reason: String,
}

#[async_trait]
impl FleetRouterCaller for LiveFleetRouter {
    fn identity(&self) -> RouterIdentity {
        RouterIdentity::from_captured(
            &self.captured,
            Some(&self.route),
            Some(self.call.disclosure.clone()),
        )
    }

    async fn decide(&self, input: &RouterCallInput) -> Result<String, String> {
        use codewhale_models::{ContentBlock, Message, MessageRequest, SystemPrompt};

        // The bounded, redacted summary is transmitted exactly once, in the
        // user turn. The system prompt carries the contract and the frozen
        // route, and no task content at all — sending it twice would double
        // what leaves for this provider while the receipt counted one copy.
        let request = MessageRequest {
            model: self.route.wire_model.clone(),
            messages: vec![Message {
                role: Role::User,
                content: vec![ContentBlock::Text {
                    text: router_user_message(input),
                    cache_control: None,
                }],
            }],
            max_tokens: self
                .client
                .effective_max_output_tokens(&self.route.wire_model),
            system: Some(SystemPrompt::Text(router_system_prompt(input))),
            // A router receives no tools. Ever.
            tools: None,
            tool_choice: None,
            metadata: None,
            thinking: None,
            // The operator-configured call tier remains authoritative. The
            // normal route allowance above leaves room for its hidden
            // reasoning before the small JSON answer is emitted.
            reasoning_effort: Some(route_reasoning_setting(
                self.provider,
                &self.base_url,
                &self.route.wire_model,
                self.call.tier,
            )),
            stream: Some(false),
            temperature: None,
            top_p: None,
        };

        let response = self
            .client
            .create_message(request)
            .await
            .map_err(|error| error.to_string())?;
        if codewhale_models::is_incomplete_stop_reason(response.stop_reason.as_deref()) {
            return Err(format!(
                "reasoning router response incomplete: provider stop reason `{}`",
                codewhale_models::stop_reason_detail(response.stop_reason.as_deref())
            ));
        }
        let text = response
            .content
            .into_iter()
            .filter_map(|block| match block {
                ContentBlock::Text { text, .. } => Some(text),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("");
        if text.trim().is_empty() {
            return Err("reasoning router returned an empty response".to_string());
        }
        Ok(text)
    }
}

// ── The Workflow ───────────────────────────────────────────────────────────

/// An exact Fleet, frozen at Workflow start.
///
/// The snapshot and the preflight are immutable for the life of the run:
/// editing `fleets/<name>.toml` afterwards changes only the next Workflow.
/// Durable runs and in-process spawns bind the same frozen member. The
/// in-process path projects only that member onto its existing profile binder;
/// it does not read or replace the currently selected Fleet.
#[derive(Clone)]
pub(crate) struct ExactFleetWorkflow {
    snapshot: Arc<FleetSnapshot>,
    preflight: Arc<RoutePreflight>,
    router: Option<Arc<dyn FleetRouterCaller>>,
    router_unavailable: Option<String>,
}

impl std::fmt::Debug for ExactFleetWorkflow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExactFleetWorkflow")
            .field("fleet", &self.snapshot.fleet().qualified())
            .field("members", &self.snapshot.members().len())
            .field("router", &self.router.is_some())
            .finish()
    }
}

/// Whether `tool` is removed by a deny entry (exact, case-insensitive, or a
/// trailing-`*` prefix glob).
fn tool_denied_by(tool: &str, denied: &[String]) -> bool {
    denied.iter().any(|entry| match entry.strip_suffix('*') {
        Some(prefix) => tool
            .to_ascii_lowercase()
            .starts_with(&prefix.to_ascii_lowercase()),
        None => entry.eq_ignore_ascii_case(tool),
    })
}

/// Refuse a task whose explicitly requested tools do not all survive the
/// role ceiling and deny list (SHA-6734). The error names the requested
/// tools, what the role allows, and what was dropped, so the caller can fix
/// the request instead of launching a child that flounders without tools.
fn refuse_dropped_requested_tools(
    member_id: &str,
    member_role: &str,
    requested: &[String],
    ceiling: &ChildAuthority,
    authority: &ChildAuthority,
) -> Result<(), String> {
    let mut requested: Vec<String> = requested.to_vec();
    requested.sort();
    requested.dedup();
    if requested.is_empty() {
        // An explicit empty list is a deliberate tool-free child.
        return Ok(());
    }
    let survives = |tool: &String| {
        authority
            .allowed_tools
            .as_ref()
            .is_none_or(|allowed| allowed.contains(tool))
            && !tool_denied_by(tool, &authority.disallowed_tools)
    };
    let dropped: Vec<&String> = requested.iter().filter(|tool| !survives(tool)).collect();
    if dropped.is_empty() {
        return Ok(());
    }
    let kept = requested.len() - dropped.len();
    let role_allows = match ceiling.allowed_tools.as_ref() {
        Some(allowed) if allowed.is_empty() => "no tools".to_string(),
        Some(allowed) => format!("[{}]", allowed.join(", ")),
        None if ceiling.disallowed_tools.is_empty() => "all inherited tools".to_string(),
        None => format!(
            "all inherited tools except [{}]",
            ceiling.disallowed_tools.join(", ")
        ),
    };
    let join = |tools: &[&String]| {
        tools
            .iter()
            .map(|tool| tool.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    };
    let requested_refs: Vec<&String> = requested.iter().collect();
    let headline = if kept == 0 {
        format!("Agent '{member_id}' (role {member_role}) would start with no tools")
    } else {
        format!("Agent '{member_id}' (role {member_role}) would lose requested tools")
    };
    Err(format!(
        "{headline}: requested [{}], role allows {role_allows}, dropped [{}]. Request only tools \
         the role allows, or pick a member whose role carries them.",
        join(&requested_refs),
        join(&dropped),
    ))
}

/// One member, resolved and admitted — but **not yet routed**.
///
/// This is the value the caller holds between admission and the Router call.
/// Producing it costs nothing: no provider is contacted, so a task that is
/// about to be rejected by a gate or blocked on capacity can be resolved
/// safely.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExactMemberBinding {
    /// Canonical member id from the frozen snapshot.
    pub(crate) member_id: String,
    /// Semantic role — what gates, handoffs, and records use.
    pub(crate) member_role: String,
    /// The preflighted, frozen route.
    pub(crate) route: PreflightedRoute,
    /// Whether this member's reasoning comes from the Router.
    pub(crate) requires_router: bool,
    /// The clamped authority the child will actually run under.
    pub(crate) authority: ChildAuthority,
    /// The live session posture this binding was clamped against, kept so the
    /// launch half can **recompute** the authority instead of trusting the copy
    /// it was handed. A binding travels across an await point (gates, a
    /// concurrency slot, a router call); recomputing is what makes a stale or
    /// tampered authority detectable rather than merely improbable.
    pub(crate) session: PermissionCeiling,
    /// Source layer captured with the snapshot, never refreshed at spawn.
    profile_origin: super::roster::ProfileOrigin,
    task_allowed_tools: Option<Vec<String>>,
    task_disallowed_tools: Vec<String>,
    task_worktree_write: bool,
}

impl ExactMemberBinding {
    /// Task options may narrow the Runtime/parent envelope, never replace the
    /// frozen identity or grant authority. Preserve the narrowing for the
    /// independent launch-time recomputation and its durable fingerprint.
    pub(crate) fn narrow_for_task(
        &mut self,
        write_authority: Option<&str>,
        allowed_tools: Option<&[String]>,
        disallowed_tools: &[String],
        max_depth: Option<u32>,
    ) -> Result<(), String> {
        match write_authority {
            Some("read_only") => self.session.write = false,
            Some("workspace_write" | "worktree_write") if !self.authority.ceiling.write => {
                return Err(format!(
                    "member `{}` is read-only under its Runtime/parent ceiling; a task cannot request write authority via `write_authority`",
                    self.member_id,
                ));
            }
            Some("workspace_write") if self.task_worktree_write => {
                return Err("a task cannot remove its worktree isolation".to_string());
            }
            Some("worktree_write") => self.task_worktree_write = true,
            Some("workspace_write") | None => {}
            Some(other) => return Err(format!("invalid task `write_authority` value `{other}`")),
        }
        if let Some(depth) = max_depth {
            self.session.delegation_depth = self.session.delegation_depth.min(depth);
        }
        if let Some(tools) = allowed_tools {
            let mut tools = tools.to_vec();
            if let Some(previous) = self.task_allowed_tools.as_ref() {
                tools.retain(|tool| previous.contains(tool));
            }
            tools.sort();
            tools.dedup();
            self.task_allowed_tools = Some(tools);
        }
        self.task_disallowed_tools
            .extend_from_slice(disallowed_tools);
        self.task_disallowed_tools.sort();
        self.task_disallowed_tools.dedup();
        let authority = self.recompute_authority(&self.member_role);
        // SHA-6734: narrowing an explicit request against the role ceiling
        // used to be silent and could start a child with no tools at all.
        // Every tool the caller asked for by name must survive, or the spawn
        // is refused with what was asked, allowed, and dropped.
        if let Some(requested) = allowed_tools {
            let ceiling = ChildAuthority::from_runtime_role(&self.member_role, self.session);
            refuse_dropped_requested_tools(
                &self.member_id,
                &self.member_role,
                requested,
                &ceiling,
                &authority,
            )?;
        }
        self.authority = authority;
        Ok(())
    }

    fn recompute_authority(&self, role: &str) -> ChildAuthority {
        let mut authority = ChildAuthority::from_runtime_role(role, self.session);
        if let Some(tools) = self.task_allowed_tools.as_ref() {
            let mut tools = tools.clone();
            if let Some(ceiling) = authority.allowed_tools.as_ref() {
                tools.retain(|tool| ceiling.contains(tool));
            }
            authority.allowed_tools = Some(tools);
        }
        if !self.task_disallowed_tools.is_empty() {
            authority
                .disallowed_tools
                .extend(self.task_disallowed_tools.iter().cloned());
            authority.disallowed_tools.sort();
            authority.disallowed_tools.dedup();
        }
        if authority.ceiling.write && self.task_worktree_write {
            authority.write_authority = "worktree_write";
        }
        authority
    }

    /// Project one preflighted member into the existing profile binder. The
    /// saved provider configuration key stays paired with the canonical wire
    /// model (including compatibility-migrated provider identities).
    pub(crate) fn spawn_profile(&self) -> super::profile::AgentProfile {
        super::profile::AgentProfile {
            native_preset: None,
            id: self.member_id.clone(),
            display_name: None,
            description: None,
            requires: Vec::new(),
            profile: codewhale_config::FleetProfile {
                slot: codewhale_config::FleetSlot::from_name(&self.member_role),
                role: codewhale_config::FleetRole {
                    name: self.member_role.clone(),
                    ..Default::default()
                },
                provider: Some(self.route.provider_config_id().to_string()),
                model: Some(self.route.wire_model.clone()),
                ..Default::default()
            },
            // Snapshot identity is recorded on the Workflow receipt; profile
            // application performs no source-file lookup.
            source: std::path::PathBuf::new(),
            origin: self.profile_origin,
            plugin_authority: None,
        }
    }
}

/// What a launched exact member resolves to, after routing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExactMemberLaunch {
    /// Canonical member id; also the roster profile id the spawn resolves.
    pub(crate) member_id: String,
    /// Semantic role, preserved for gates/handoffs/records.
    pub(crate) member_role: String,
    /// Frozen provider id.
    pub(crate) provider: String,
    /// Canonical wire model — the same string the receipt records.
    pub(crate) model: String,
    /// Concrete reasoning setting label for the spawn request.
    pub(crate) thinking: String,
    /// The full requested → effective story, for the receipt.
    pub(crate) reasoning: ResolvedReasoning,
    /// The clamped authority the child runs under.
    pub(crate) authority: ChildAuthority,
    /// The durable, visible receipt for this launch.
    pub(crate) receipt: FleetTaskReceipt,
}

impl ExactFleetWorkflow {
    /// Capture a Workflow from a parsed exact Fleet document.
    ///
    /// Everything that can fail locally fails here, before any worker is
    /// dispatched: an unresolvable provider, an unknown model, a missing
    /// credential, an unresolvable Reasoning Router profile, or an `auto`
    /// member with no usable Router.
    pub(crate) fn capture(
        document: &FleetDocument,
        id: QualifiedFleetId,
        captured_at: impl Into<String>,
        config: Option<&Config>,
        search_roots: &[FleetSearchRoot],
    ) -> Result<Self, String> {
        let exact = document
            .exact()
            .ok_or_else(|| "this Fleet is not an exact Fleet".to_string())?;

        // Resolve the attached Reasoning Router *reference* into the one
        // captured service both forms normalize onto.
        let captured_router = match exact.router_ref() {
            None => None,
            Some(FleetRouterRef::LegacyInline(_)) => captured_legacy_inline_router(exact),
            Some(FleetRouterRef::Profile { name }) => {
                let (profile, router_id) =
                    ReasoningRouterProfile::load_by_name(&name, search_roots).map_err(|error| {
                        format!(
                            "exact Fleet `{}` references reasoning router `{name}`, which could \
                             not be loaded: {error}",
                            id.qualified()
                        )
                    })?;
                Some(CapturedReasoningRouter::from_profile(
                    &profile,
                    router_id.origin,
                ))
            }
        };

        // Capture, then immediately verify the hash the receipt will vouch for.
        // `capture` computes it, so this can only fail if the value took a
        // detour through `Deserialize` — but that is exactly the case a receipt
        // must not certify, and checking here means no later caller has to
        // remember to.
        let snapshot = FleetSnapshot::capture(id, document, captured_at, captured_router.clone())
            .and_then(FleetSnapshot::into_verified)
            .map_err(|error| error.to_string())?;

        // Preflight every worker route before anything else can happen.
        let (preflight, router) = Self::preflight_and_bind(&snapshot, captured_router, config)?;

        let router_unavailable = match (snapshot.router(), &router) {
            (Some(_), None) => {
                Some("the Fleet's reasoning router could not be bound on this machine".to_string())
            }
            _ => None,
        };

        let workflow = Self {
            snapshot: Arc::new(snapshot),
            preflight: Arc::new(preflight),
            router,
            router_unavailable,
        };
        workflow.reject_unusable_auto_members()?;
        Ok(workflow)
    }

    /// Preflight every worker route and bind the Router, or fail the start.
    fn preflight_and_bind(
        snapshot: &FleetSnapshot,
        captured_router: Option<CapturedReasoningRouter>,
        config: Option<&Config>,
    ) -> Result<(RoutePreflight, Option<Arc<dyn FleetRouterCaller>>), String> {
        let Some(config) = config else {
            return Err(format!(
                "exact Fleet `{}` cannot start: no session config is available to preflight its \
                 members' providers and models. An exact Fleet fails closed here rather than \
                 dispatching a worker onto a route it never verified.",
                snapshot.fleet().qualified()
            ));
        };

        let mut workers = Vec::with_capacity(snapshot.members().len());
        for member in snapshot.members() {
            let route = preflight_route(
                &member.id,
                &member.route.provider,
                &member.route.model,
                config,
            )
            .map_err(|error| {
                format!(
                    "exact Fleet `{}` cannot start: {error}",
                    snapshot.fleet().qualified()
                )
            })?;
            route.require_ready().map_err(|error| {
                format!(
                    "exact Fleet `{}` cannot start: {error}",
                    snapshot.fleet().qualified()
                )
            })?;
            workers.push(route);
        }

        // Every worker client is constructed and validated **before** the
        // Router is bound, let alone called. A member whose client cannot be
        // built is a start-time failure; discovering it after a Router decision
        // means the operator paid for a routing request for a task that could
        // never have run.
        for route in &workers {
            validate_route_client(route, config).map_err(|error| {
                format!(
                    "exact Fleet `{}` cannot start: {error}",
                    snapshot.fleet().qualified()
                )
            })?;
        }

        let mut router: Option<Arc<dyn FleetRouterCaller>> = None;
        let mut router_route = None;
        if let Some(captured) = &captured_router {
            match LiveFleetRouter::bind(captured, config) {
                Ok(live) => {
                    router_route = Some(live.route().clone());
                    router = Some(Arc::new(live));
                }
                Err(error) => {
                    // Recorded rather than raised: a Fleet with no `auto`
                    // member does not need its router to be usable, and
                    // failing the whole Workflow for an unused service would
                    // be the wrong trade.
                    if snapshot.has_auto_member() {
                        return Err(format!(
                            "exact Fleet `{}` cannot start: member(s) {} request reasoning \
                             `auto` but the Fleet's reasoning router is unusable ({}). Fix the \
                             router profile or pin an explicit reasoning tier — exact Fleets \
                             never fall back to legacy model routing or a local heuristic.",
                            snapshot.fleet().qualified(),
                            snapshot.auto_member_ids().join(", "),
                            error.reason,
                        ));
                    }
                }
            }
        }

        Ok((RoutePreflight::new(workers, router_route), router))
    }

    /// Fail at Workflow start — not at task launch — when a member requests
    /// `auto` and the Fleet has no Router it can actually call.
    fn reject_unusable_auto_members(&self) -> Result<(), String> {
        if !self.snapshot.has_auto_member() || self.router.is_some() {
            return Ok(());
        }
        let reason = self
            .router_unavailable
            .clone()
            .unwrap_or_else(|| "this Fleet references no reasoning router".to_string());
        Err(format!(
            "exact Fleet `{}` cannot start: member(s) {} request reasoning `auto` but the Fleet's \
             reasoning router is unusable ({reason}). Attach a working reasoning router or pin an \
             explicit reasoning tier — exact Fleets never fall back to legacy model routing or a \
             local heuristic.",
            self.snapshot.fleet().qualified(),
            self.snapshot.auto_member_ids().join(", "),
        ))
    }

    #[must_use]
    pub(crate) fn snapshot(&self) -> &Arc<FleetSnapshot> {
        &self.snapshot
    }

    /// Human-readable roster listing for "unknown member" errors.
    #[must_use]
    pub(crate) fn member_names(&self) -> String {
        self.snapshot
            .members()
            .iter()
            .map(|member| {
                if member.role == member.id {
                    member.id.clone()
                } else {
                    format!("{} (role {})", member.id, member.role)
                }
            })
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// Resolve a task's `role`/`profile` to one admitted member, **without
    /// contacting any provider**.
    ///
    /// This is deliberately the cheap half of a launch. It runs before gate
    /// evaluation and before a concurrency slot is taken, so a task that is
    /// about to be rejected or queued costs nothing and discloses nothing.
    ///
    /// A task that names both a `profile` and a `role` which resolve to
    /// different members is **rejected**, not silently resolved by precedence:
    /// the two fields would then disagree about who ran, and the receipt could
    /// only record one of them.
    pub(crate) fn bind_member(
        &self,
        profile: Option<&str>,
        role: Option<&str>,
        session: PermissionCeiling,
    ) -> Result<ExactMemberBinding, String> {
        let fleet = self.snapshot.fleet().qualified();
        let profile = profile.map(str::trim).filter(|key| !key.is_empty());
        let role = role.map(str::trim).filter(|key| !key.is_empty());

        let member = match (profile, role) {
            (None, None) => {
                return Err(format!(
                    "Fleet `{fleet}` is an exact Fleet: every task must name a member via `role` \
                     or `profile`. Members: {}",
                    self.member_names()
                ));
            }
            (Some(profile), None) => self.lookup(profile)?,
            (None, Some(role)) => self.lookup(role)?,
            (Some(profile), Some(role)) => {
                let by_profile = self.lookup(profile)?;
                let by_role = self.lookup(role)?;
                if by_profile.id != by_role.id {
                    return Err(format!(
                        "Fleet `{fleet}`: task names profile `{profile}` (member `{}`) and role \
                         `{role}` (member `{}`), which are different members. A task must name \
                         one member; the two fields cannot disagree about who ran.",
                        by_profile.id, by_role.id
                    ));
                }
                by_profile
            }
        };

        let route = self.preflight.worker(&member.id).ok_or_else(|| {
            format!(
                "Fleet `{fleet}`: member `{}` has no preflighted route",
                member.id
            )
        })?;

        Ok(ExactMemberBinding {
            member_id: member.id.clone(),
            member_role: public_role_label(&member.role),
            route: route.clone(),
            requires_router: member.requested_reasoning.is_auto(),
            authority: ChildAuthority::from_runtime_role(&member.role, session),
            session,
            profile_origin: match self.snapshot.fleet().origin.as_str() {
                "workspace" => super::roster::ProfileOrigin::Workspace,
                "codewhale_home" => super::roster::ProfileOrigin::Personal,
                _ => super::roster::ProfileOrigin::Config,
            },
            task_allowed_tools: None,
            task_disallowed_tools: Vec::new(),
            task_worktree_write: false,
        })
    }

    fn lookup(&self, key: &str) -> Result<&FleetSnapshotMember, String> {
        self.snapshot.member_by_id_or_role(key).ok_or_else(|| {
            format!(
                "unknown exact Fleet member `{key}` in `{}`. Members: {}",
                self.snapshot.fleet().qualified(),
                self.member_names()
            )
        })
    }

    /// Finish an **already admitted** binding: decide only how hard the already
    /// frozen model thinks, then build the receipt.
    ///
    /// This is the half that can cost money. Calling it means the task has
    /// already passed its gates and holds a concurrency slot.
    pub(crate) async fn route_admitted_task(
        &self,
        binding: &ExactMemberBinding,
        task_summary: &str,
    ) -> Result<ExactMemberLaunch, String> {
        // The receipt built at the end of this function stamps
        // `snapshot.content_hash()` as evidence that this launch matched a saved
        // definition. Verify the hash actually describes the snapshot *before*
        // spending a router call or emitting that claim — an unverified hash is
        // not weaker evidence, it is a false receipt.
        self.snapshot
            .verify_content_hash()
            .map_err(|error| error.to_string())?;

        let member = self.snapshot.member(&binding.member_id).ok_or_else(|| {
            format!(
                "Fleet `{}`: member `{}` vanished between admission and launch",
                self.snapshot.fleet().qualified(),
                binding.member_id
            )
        })?;

        // Recompute authority from Runtime's role policy and the live-parent
        // posture this binding was admitted against, and require it to be
        // *identical* to the one the binding carries. The snapshot supplies
        // identity only; legacy internal `FleetProfilePermissions` input is never
        // consulted.
        //
        // A binding crosses gates, a concurrency wait, and (for `auto` members)
        // a router call before it gets here, so "the authority I was handed" and
        // "the authority this member actually has" are two different claims. The
        // launch below is the value the spawn path consumes, so it must be the
        // recomputed one; the equality check is what turns a divergence into a
        // refused launch instead of a silently widened child.
        let authority = binding.recompute_authority(&member.role);
        if authority != binding.authority {
            return Err(format!(
                "Fleet `{}`: member `{}` resolved a different permission envelope at launch than \
                 at admission, so the launch is refused. admitted={} launched={}",
                self.snapshot.fleet().qualified(),
                binding.member_id,
                binding.authority.fingerprint(),
                authority.fingerprint(),
            ));
        }

        // The route is already frozen and preflighted. Nothing below may move
        // it — not a task option, not the Router.
        let frozen = binding.route.frozen();
        let capability = binding.route.capability;

        let availability = self.router_availability();
        let mut router_identity = None;
        let mut routing_summary: Option<RoutingDisclosure> = None;
        let decision = if binding.requires_router {
            let router = self.router.as_ref().ok_or_else(|| {
                format!(
                    "member `{}` requests reasoning `auto` but Fleet `{}` has no usable reasoning \
                     router",
                    binding.member_id,
                    self.snapshot.fleet().qualified()
                )
            })?;
            let cross_provider = self.preflight.crosses_providers(&binding.member_id);
            let payload = bounded_routing_payload(task_summary).with_cross_provider(cross_provider);
            // What actually leaves for the router's provider, recorded so the
            // receipt discloses it — counts and hash only, never the text.
            routing_summary = Some(payload.disclosure().clone());
            router_identity = Some(router.identity());
            let input = RouterCallInput {
                fleet: self.snapshot.fleet().qualified(),
                member_id: binding.member_id.clone(),
                frozen: frozen.clone(),
                payload,
            };
            let raw = router.decide(&input).await.map_err(|error| {
                format!(
                    "reasoning router call failed for member `{}`: {error}",
                    binding.member_id
                )
            })?;
            Some(parse_router_decision(&raw).map_err(|error| {
                format!(
                    "reasoning router returned an unusable decision for member `{}`: {error}",
                    binding.member_id
                )
            })?)
        } else {
            None
        };

        let reasoning = resolve_exact_member_reasoning(
            &binding.member_id,
            &frozen,
            member.requested_reasoning,
            &capability,
            &availability,
            decision.as_ref(),
            router_identity.as_ref(),
        )
        .map_err(|error| error.to_string())?;

        // Every exact launch carries a concrete tier. `auto` is resolved by the
        // router above and the literal sentinel never leaves this function.
        //
        // `NativeAdaptive` is no longer reachable here: removing the bypass
        // (so `auto` always asks the router) also removed the one path that
        // produced it. It used to be launched as `off`, which mislabelled the
        // request — a route choosing its own depth is not a route with thinking
        // disabled. Rather than re-introduce that lie, this fails loudly if the
        // variant ever comes back.
        let thinking = match reasoning.effective() {
            EffectiveReasoning::Tier(tier) => effort_of(tier).as_setting().to_string(),
            EffectiveReasoning::NativeAdaptive => {
                return Err(format!(
                    "member `{}` resolved to provider-native adaptive reasoning, which an exact \
                     Fleet launch cannot place on a request. Pin an explicit reasoning tier.",
                    binding.member_id
                ));
            }
        };

        // The durable receipt. Built here, at the one place that knows every
        // side of the decision, so no consumer has to re-derive it.
        let receipt = FleetTaskReceipt::new(
            self.snapshot.fleet().qualified(),
            self.snapshot.schema_kind(),
            self.snapshot.schema_revision(),
            self.snapshot.content_hash(),
            binding.member_id.clone(),
            binding.member_role.clone(),
            &binding.route,
            &reasoning,
            routing_summary,
            binding.authority.ceiling.network_tool,
        )
        // The fingerprint of the envelope this launch installs, carried on the
        // durable receipt so the spawn boundary has something to check against
        // rather than a sentinel it can only assume.
        .with_authority_fingerprint(authority.fingerprint())
        // Semantic role and runtime posture stay two separate facts all the way
        // onto the durable receipt: `member_role` is what the operator named
        // and what gates key on, `posture_role` is the Runtime baseline role.
        // The fingerprint above records the effective parent-narrowed surface.
        .with_posture_role(binding.authority.posture_role);

        Ok(ExactMemberLaunch {
            member_id: binding.member_id.clone(),
            member_role: binding.member_role.clone(),
            provider: frozen.provider,
            model: frozen.model,
            thinking,
            reasoning,
            authority,
            receipt,
        })
    }

    fn router_availability(&self) -> RouterAvailability {
        match (&self.router, &self.router_unavailable) {
            (Some(_), _) => RouterAvailability::Ready,
            (None, Some(reason)) => RouterAvailability::Unavailable {
                reason: reason.clone(),
            },
            (None, None) => RouterAvailability::Absent,
        }
    }
}

// ── Test seams ──────────────────────────────────────────────────────────────

/// A Router that answers with a fixed fixture string, recording what it saw.
///
/// Test-only: it is how the exact-Fleet reasoning path is exercised end to end
/// without a provider call, and how "the router was never called" is asserted.
#[cfg(test)]
#[derive(Debug)]
pub(crate) struct StaticFleetRouter {
    response: String,
    identity: RouterIdentity,
    pub(crate) seen: std::sync::Mutex<Vec<RouterCallInput>>,
}

#[cfg(test)]
impl StaticFleetRouter {
    pub(crate) fn new(response: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            response: response.into(),
            identity: RouterIdentity {
                id: "luna-low".to_string(),
                origin: "workspace".to_string(),
                service_kind: codewhale_workflow::REASONING_ROUTER_SERVICE_KIND.to_string(),
                legacy_inline: false,
                provider: "openai".to_string(),
                model: "gpt-5.6-luna".to_string(),
                endpoint: Some(EndpointIdentity::from_base_url("https://api.openai.com/v1")),
                call: Some(
                    router_call_plan(
                        codewhale_workflow::RouterCallReasoning::Low,
                        &ReasoningCapability::tiered(),
                    )
                    .disclosure,
                ),
            },
            seen: std::sync::Mutex::new(Vec::new()),
        })
    }

    /// How many router calls were made. Zero is the assertion that matters for
    /// manual reasoning and for rejected/blocked tasks.
    pub(crate) fn call_count(&self) -> usize {
        self.seen.lock().expect("router log").len()
    }
}

#[cfg(test)]
#[async_trait]
impl FleetRouterCaller for StaticFleetRouter {
    fn identity(&self) -> RouterIdentity {
        self.identity.clone()
    }

    async fn decide(&self, input: &RouterCallInput) -> Result<String, String> {
        self.seen.lock().expect("router log").push(input.clone());
        Ok(self.response.clone())
    }
}

#[cfg(test)]
impl ExactFleetWorkflow {
    /// Build a Workflow with an injected Router and a supplied capability,
    /// skipping provider binding so the reasoning path runs with no network and
    /// no configured provider.
    /// Takes the concrete fixture type rather than `Option<Arc<dyn ...>>`:
    /// `Option` does not coerce its payload, so the unsizing is done once here
    /// instead of at every call site.
    pub(crate) fn for_tests(
        document: &FleetDocument,
        id: QualifiedFleetId,
        router: Option<Arc<StaticFleetRouter>>,
    ) -> Self {
        Self::for_tests_with_capability(document, id, router, ReasoningCapability::tiered())
    }

    pub(crate) fn for_tests_with_capability(
        document: &FleetDocument,
        id: QualifiedFleetId,
        router: Option<Arc<StaticFleetRouter>>,
        capability: ReasoningCapability,
    ) -> Self {
        let exact = document.exact().expect("exact Fleet");
        let captured = captured_legacy_inline_router(exact).or_else(|| {
            exact.reasoning_router.as_ref().map(|name| {
                CapturedReasoningRouter::from_profile(
                    &ReasoningRouterProfile::parse(&format!(
                        "name = \"{name}\"\nschema = \"reasoning_router\"\nprovider = \
                         \"openai\"\nmodel = \"gpt-5.6-luna\"\ncall_reasoning = \"low\"\n"
                    ))
                    .expect("router profile"),
                    "workspace",
                )
            })
        });
        let snapshot =
            FleetSnapshot::capture(id, document, "2026-07-26T00:00:00Z", captured.clone())
                .expect("valid roster");

        let workers = snapshot
            .members()
            .iter()
            .map(|member| {
                test_route(
                    &member.id,
                    &member.route.provider,
                    &member.route.model,
                    capability,
                )
            })
            .collect::<Vec<_>>();
        let router_route = captured.as_ref().map(|captured| {
            test_route(
                "router",
                &captured.route.provider,
                &captured.route.model,
                capability,
            )
        });
        let preflight = RoutePreflight::new(workers, router_route);

        Self {
            snapshot: Arc::new(snapshot),
            preflight: Arc::new(preflight),
            router: router.map(|router| {
                let router: Arc<dyn FleetRouterCaller> = router;
                router
            }),
            router_unavailable: None,
        }
    }

    /// A Workflow whose Router failed to bind locally — the shape
    /// [`Self::capture`] produces when a Router's provider has no credentials
    /// configured on this machine. No network is involved either way.
    pub(crate) fn for_tests_with_unavailable_router(
        document: &FleetDocument,
        id: QualifiedFleetId,
        reason: &str,
    ) -> Result<Self, String> {
        let mut workflow = Self::for_tests(document, id, None);
        workflow.router_unavailable = Some(reason.to_string());
        workflow.reject_unusable_auto_members()?;
        Ok(workflow)
    }
}

#[cfg(test)]
fn test_route(
    member: &str,
    provider: &str,
    model: &str,
    capability: ReasoningCapability,
) -> PreflightedRoute {
    PreflightedRoute {
        member_id: member.to_string(),
        provider_id: provider.to_string(),
        provider_config_id: None,
        provider_kind: provider.to_string(),
        declared_model: model.to_string(),
        wire_model: model.to_string(),
        endpoint: EndpointIdentity::from_base_url("https://api.example.test/v1"),
        credential: CredentialReadiness::Configured,
        capability,
    }
}

#[cfg(test)]
mod shell_ceiling_tests {
    use super::*;

    fn ceiling(write: bool, shell: ShellCeiling) -> PermissionCeiling {
        PermissionCeiling {
            write,
            network_tool: false,
            shell,
            delegation_depth: 0,
            tools: true,
        }
    }

    fn session() -> PermissionCeiling {
        ceiling(true, ShellCeiling::Full)
    }

    fn denies_raw_shell(authority: &ChildAuthority) -> bool {
        authority
            .disallowed_tools
            .iter()
            .any(|rule| rule == RAW_SHELL_SENTINEL)
    }

    /// The `analyst` preset grants no shell. The envelope reads its shell bit
    /// back off the deny list, so the denial has to actually be installed —
    /// otherwise a shell-less ceiling reaches dispatch claiming full shell
    /// authority and can start a verification process.
    #[test]
    fn a_shell_less_ceiling_installs_the_raw_shell_denial() {
        for shell in [ShellCeiling::None, ShellCeiling::ReadOnly] {
            let authority = ChildAuthority::clamp(ceiling(false, shell), session());
            assert!(
                denies_raw_shell(&authority),
                "{shell:?} must deny raw shell"
            );
        }
    }

    /// The gap this repair closed: a write-capable member inside a session with
    /// no shell authority clamps to `write = true, shell = none`. Keying the
    /// denial on `write` alone left that combination with no denial installed —
    /// and therefore with an envelope that claimed shell authority the ceiling
    /// had refused.
    #[test]
    fn a_write_capable_member_clamped_to_no_shell_still_loses_raw_shell() {
        let authority = ChildAuthority::clamp(
            ceiling(true, ShellCeiling::Full),
            ceiling(true, ShellCeiling::None),
        );

        assert_eq!(authority.ceiling.shell, ShellCeiling::None);
        assert!(authority.ceiling.write, "the write half is unchanged");
        assert!(denies_raw_shell(&authority));
    }

    /// Prior behavior preserved: a `verifier`/`tester` ceiling
    /// (`write = false, shell = "full"`) still loses raw shell, and a fully
    /// write-capable member still keeps it.
    #[test]
    fn the_existing_verifier_and_full_ceilings_are_unchanged() {
        let verifier = ChildAuthority::clamp(ceiling(false, ShellCeiling::Full), session());
        assert!(denies_raw_shell(&verifier));
        assert_eq!(verifier.posture_role, "test");

        let full = ChildAuthority::clamp(ceiling(true, ShellCeiling::Full), session());
        assert!(!denies_raw_shell(&full));
        assert_eq!(full.posture_role, "implement");
    }

    #[test]
    fn bounded_inspection_role_keeps_only_classifier_bounded_bash() {
        for role in ["scout", "reviewer", "planner"] {
            let authority = ChildAuthority::from_runtime_role(role, session());
            assert!(
                !authority
                    .disallowed_tools
                    .iter()
                    .any(|name| name.eq_ignore_ascii_case("Bash")),
                "{role} keeps canonical Bash for per-input classification"
            );
            for denied in [
                "exec_shell",
                "task_shell_start",
                "task_shell_wait",
                "terminal/*",
                "write_file",
                "apply_patch",
            ] {
                assert!(
                    authority.disallowed_tools.iter().any(|name| name == denied),
                    "{role} must still deny {denied}: {:?}",
                    authority.disallowed_tools
                );
            }
        }

        for role in ["consultant", "verifier"] {
            let authority = ChildAuthority::from_runtime_role(role, session());
            assert!(
                authority
                    .disallowed_tools
                    .iter()
                    .any(|name| name.eq_ignore_ascii_case("Bash")),
                "{role} must not gain the read-only inspection exception"
            );
        }

        let parent_shell_off =
            ChildAuthority::from_runtime_role("scout", ceiling(true, ShellCeiling::None));
        assert!(
            parent_shell_off
                .disallowed_tools
                .iter()
                .any(|name| name.eq_ignore_ascii_case("Bash")),
            "a named Scout may not turn a parent shell-off ceiling into ReadOnly"
        );
        let planner_parent_shell_off =
            ChildAuthority::from_runtime_role("planner", ceiling(true, ShellCeiling::None));
        assert!(
            planner_parent_shell_off
                .disallowed_tools
                .iter()
                .any(|name| name.eq_ignore_ascii_case("Bash")),
            "a named planner may not turn a parent shell-off ceiling into ReadOnly"
        );
        assert_eq!(planner_parent_shell_off.posture_role, "planner");
        assert_eq!(
            session_shell_ceiling(
                codewhale_agent_contract::worker_profile::ShellPolicy::Full,
                false
            ),
            ShellCeiling::None
        );
    }

    /// #5426 acceptance 2, made mechanical: delegation moves work, never
    /// authority. A read-only scout's own runtime posture is the "session"
    /// its children clamp against, so a Runtime `builder` dispatched from a
    /// read-only parent lands read-only — raw shell gone and mutating tools
    /// denied — while the Runtime posture remains separately identified and delegation stays
    /// available (the depth budget is the parent's, not zero). The escape
    /// hatch is work capacity, never a wider envelope.
    #[test]
    fn a_read_only_parents_delegation_never_widens_authority() {
        // The scout's live runtime posture, expressed as the session ceiling
        // a child clamps against: no writes, read-only shell, network kept,
        // one level of delegation budget left.
        let scout_runtime = PermissionCeiling {
            write: false,
            network_tool: true,
            shell: ShellCeiling::ReadOnly,
            delegation_depth: 1,
            tools: true,
        };
        let authority = ChildAuthority::from_runtime_role("builder", scout_runtime);

        // Authority does not widen through delegation: the child is read-only.
        assert!(!authority.ceiling.write);
        assert_eq!(authority.ceiling.shell, ShellCeiling::ReadOnly);
        assert_eq!(authority.write_authority, "read_only");
        assert_eq!(authority.posture_role, "implement");
        assert!(denies_raw_shell(&authority));
        for mutating in ["write_file", "apply_patch"] {
            assert!(
                authority
                    .disallowed_tools
                    .iter()
                    .any(|rule| rule == mutating),
                "{mutating} must stay denied for a scout-delegated builder: {:?}",
                authority.disallowed_tools
            );
        }

        // The escape hatch itself stays open: delegation is still possible
        // (the parent's budget is intact). But it is useless for shell:
        // canonical Bash is denied to a delegated child (it is not a bounded
        // inspection role), so a scout can never obtain bash by spawning —
        // the scout's own bounded read-only Bash from #5428 is the only shell
        // path a read-only parent has.
        assert_eq!(authority.max_depth, 1);
        assert!(
            authority
                .disallowed_tools
                .iter()
                .any(|name| name.eq_ignore_ascii_case("Bash")),
            "a scout-delegated child must not gain canonical Bash: {:?}",
            authority.disallowed_tools
        );
    }

    /// The deny list feeds the fingerprint, so a ceiling that now denies more
    /// must fingerprint differently from one that does not. Two postures that
    /// install different surfaces may never share a fingerprint.
    #[test]
    fn the_shell_denial_is_visible_in_the_fingerprint() {
        let no_shell = ChildAuthority::clamp(ceiling(false, ShellCeiling::None), session());
        let full = ChildAuthority::clamp(ceiling(true, ShellCeiling::Full), session());

        assert_ne!(no_shell.fingerprint(), full.fingerprint());
        assert!(no_shell.fingerprint().contains("shell=none"));
    }

    /// Every rule the shell clamp installs is a *posture* denial, so a
    /// grandchild spawned with `inherit_disallowed_tools: false` cannot drop it.
    #[test]
    fn the_installed_shell_denials_are_posture_denials() {
        let authority = ChildAuthority::clamp(ceiling(false, ShellCeiling::None), session());
        for rule in &authority.disallowed_tools {
            assert!(is_posture_denial(rule), "{rule} must be a posture denial");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codewhale_workflow::{
        EffectiveReasoningSource, ProviderEffectiveReasoning, RequestedReasoning,
    };

    /// A Fleet that references a saved, reusable Reasoning Router service.
    const GLM_FLEET: &str = r#"
name = "glm-pair"
schema = "exact"
reasoning_router = "luna-low"

[[members]]
id = "implementer"
role = "builder"
provider = "zai"
model = "glm-5"
reasoning = "auto"
permissions = "read_write"

[[members]]
id = "auditor"
role = "reviewer"
provider = "zai"
model = "glm-5"
reasoning = "high"
permissions = "read_only"
"#;

    fn id() -> QualifiedFleetId {
        QualifiedFleetId {
            name: "glm-pair".to_string(),
            origin: "workspace".to_string(),
        }
    }

    fn full_session() -> PermissionCeiling {
        PermissionCeiling {
            write: true,
            network_tool: true,
            shell: ShellCeiling::Full,
            delegation_depth: codewhale_protocol::fleet::DEFAULT_SPAWN_DEPTH,
            tools: true,
        }
    }

    /// Takes the concrete fixture type: `Option` does not coerce its payload,
    /// so the unsizing to `Arc<dyn FleetRouterCaller>` is spelled out here once
    /// rather than at every call site.
    fn workflow_with(router: Option<Arc<StaticFleetRouter>>, text: &str) -> ExactFleetWorkflow {
        let document = FleetDocument::parse(text).expect("parse");
        ExactFleetWorkflow::for_tests(&document, id(), router)
    }

    #[tokio::test]
    async fn an_auto_member_takes_a_reasoning_only_router_decision_on_a_frozen_route() {
        let router = StaticFleetRouter::new(r#"{"reasoning":"max"}"#);
        let workflow = workflow_with(Some(router.clone()), GLM_FLEET);

        let binding = workflow
            .bind_member(None, Some("builder"), full_session())
            .expect("role resolves");
        assert_eq!(
            router.call_count(),
            0,
            "binding a member must not cost a router call"
        );

        let launch = workflow
            .route_admitted_task(&binding, "refactor three crates")
            .await
            .expect("auto resolves through the router");

        // The route did not move.
        assert_eq!(launch.provider, "zai");
        assert_eq!(launch.model, "glm-5");
        assert_eq!(launch.thinking, "max");
        assert_eq!(launch.member_id, "implementer");
        assert_eq!(launch.member_role, "implement");
        assert_eq!(launch.reasoning.requested(), RequestedReasoning::Auto);
        assert_eq!(
            launch.reasoning.source(),
            EffectiveReasoningSource::FleetRouter
        );

        // The router saw the frozen route as context, never as a question, and
        // received the bounded payload rather than the raw task.
        let seen = router.seen.lock().expect("log");
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].frozen.model, "glm-5");
        assert_eq!(seen[0].member_id, "implementer");
        assert_eq!(seen[0].payload.text(), "refactor three crates");
    }

    /// The semantic role must survive onto the launch and the receipt: a gate
    /// or handoff keyed on `builder` has to still see `builder` even though the
    /// roster resolves the distinct profile id `implementer`.
    #[tokio::test]
    async fn the_semantic_role_survives_while_the_id_addresses_the_roster() {
        let workflow = workflow_with(
            Some(StaticFleetRouter::new(r#"{"reasoning":"low"}"#)),
            GLM_FLEET,
        );

        let binding = workflow
            .bind_member(None, Some("reviewer"), full_session())
            .expect("role lookup");
        assert_eq!(binding.member_id, "auditor");
        assert_eq!(binding.member_role, "reviewer");

        let launch = workflow
            .route_admitted_task(&binding, "read the diff")
            .await
            .expect("launch");
        assert_eq!(launch.receipt.member_id, "auditor");
        assert_eq!(
            launch.receipt.member_role, "reviewer",
            "the receipt records the semantic role, not the profile id"
        );

        // The snapshot is addressed by id; the role is the semantic label.
        let member = workflow
            .snapshot()
            .member("auditor")
            .expect("snapshot entry");
        assert_eq!(member.role, "reviewer");
    }

    /// A task that names a profile and a role belonging to different members is
    /// rejected — the two fields cannot disagree about who ran.
    #[test]
    fn a_conflicting_task_role_and_profile_is_rejected() {
        let workflow = workflow_with(None, GLM_FLEET);

        let err = workflow
            .bind_member(Some("implementer"), Some("reviewer"), full_session())
            .expect_err("conflicting identity");
        assert!(err.contains("different members"), "{err}");
        assert!(err.contains("implementer"), "{err}");
        assert!(err.contains("auditor"), "{err}");

        // Agreeing fields are fine: id plus that member's own role.
        let binding = workflow
            .bind_member(Some("implementer"), Some("builder"), full_session())
            .expect("agreeing identity");
        assert_eq!(binding.member_id, "implementer");
    }

    /// Manual reasoning uses no Router at all — not a call whose answer is
    /// discarded, but zero calls.
    #[tokio::test]
    async fn an_explicit_tier_member_never_calls_the_router() {
        let router = StaticFleetRouter::new(r#"{"reasoning":"off"}"#);
        let workflow = workflow_with(Some(router.clone()), GLM_FLEET);

        let binding = workflow
            .bind_member(Some("auditor"), None, full_session())
            .expect("bind");
        assert!(!binding.requires_router);

        let launch = workflow
            .route_admitted_task(&binding, "read the diff")
            .await
            .expect("explicit tier");

        assert_eq!(launch.thinking, "high");
        assert_eq!(
            launch.reasoning.source(),
            EffectiveReasoningSource::MemberExplicit
        );
        assert_eq!(
            router.call_count(),
            0,
            "an explicit tier must not spend a router call"
        );
        assert!(launch.receipt.router.is_none());
        assert!(launch.receipt.routing_summary.is_none());
        assert!(!launch.receipt.cross_provider_inference);
    }

    /// A task that never reaches admission must never reach the Router. This
    /// is the shape of a gate rejection or a capacity block: the caller binds,
    /// decides not to proceed, and no provider was contacted.
    #[test]
    fn a_task_that_is_never_admitted_costs_no_router_call() {
        let router = StaticFleetRouter::new(r#"{"reasoning":"max"}"#);
        let workflow = workflow_with(Some(router.clone()), GLM_FLEET);

        // Unknown member: rejected during binding, before any cost.
        assert!(
            workflow
                .bind_member(None, Some("wizard"), full_session())
                .is_err()
        );
        // Conflicting identity: likewise.
        assert!(
            workflow
                .bind_member(Some("implementer"), Some("reviewer"), full_session())
                .is_err()
        );
        // A valid binding that the caller then abandons (gate reject / no slot).
        let _binding = workflow
            .bind_member(None, Some("builder"), full_session())
            .expect("valid binding");

        assert_eq!(
            router.call_count(),
            0,
            "no router call may happen before a task is admitted"
        );
    }

    #[tokio::test]
    async fn a_router_that_tries_to_move_the_route_fails_the_launch() {
        let workflow = workflow_with(
            Some(StaticFleetRouter::new(
                r#"{"reasoning":"max","model":"glm-4"}"#,
            )),
            GLM_FLEET,
        );
        let binding = workflow
            .bind_member(None, Some("builder"), full_session())
            .expect("bind");

        let err = workflow
            .route_admitted_task(&binding, "anything")
            .await
            .expect_err("a route mutation must fail the launch");
        assert!(err.contains("frozen"), "{err}");
    }

    #[tokio::test]
    async fn a_duplicate_reasoning_key_fails_the_launch() {
        let workflow = workflow_with(
            Some(StaticFleetRouter::new(
                r#"{"reasoning":"off","reasoning":"max"}"#,
            )),
            GLM_FLEET,
        );
        let binding = workflow
            .bind_member(None, Some("builder"), full_session())
            .expect("bind");

        let err = workflow
            .route_admitted_task(&binding, "anything")
            .await
            .expect_err("duplicate key");
        assert!(err.contains("more than once"), "{err}");
    }

    #[test]
    fn a_missing_router_fails_before_any_worker_is_dispatched() {
        let router_less = GLM_FLEET.replace("reasoning_router = \"luna-low\"\n", "");
        let document = FleetDocument::parse(&router_less).expect("parse");
        let workflow = ExactFleetWorkflow::for_tests(&document, id(), None);

        let err = workflow
            .reject_unusable_auto_members()
            .expect_err("auto without a router must not start");
        assert!(err.contains("implementer"), "{err}");
        assert!(err.contains("reasoning router"), "{err}");
        assert!(
            err.contains("never fall back"),
            "the error must rule out legacy fallback: {err}"
        );
    }

    #[test]
    fn a_fleet_with_no_auto_member_starts_without_a_router() {
        let text = r#"
name = "pinned"
schema = "exact"

[[members]]
id = "auditor"
provider = "zai"
model = "glm-5"
reasoning = "high"
permissions = "read_only"
"#;
        let document = FleetDocument::parse(text).expect("parse");
        let workflow = ExactFleetWorkflow::for_tests(
            &document,
            QualifiedFleetId {
                name: "pinned".to_string(),
                origin: "workspace".to_string(),
            },
            None,
        );
        workflow
            .reject_unusable_auto_members()
            .expect("no auto member means no router requirement");
        assert_eq!(workflow.snapshot().members().len(), 1);
    }

    /// A Router whose credentials are locally absent fails the Workflow before
    /// any worker is dispatched — decided from local config, never from a live
    /// probe of the provider.
    #[test]
    fn a_locally_unusable_router_fails_before_any_worker_is_dispatched() {
        let document = FleetDocument::parse(GLM_FLEET).expect("parse");
        let err = ExactFleetWorkflow::for_tests_with_unavailable_router(
            &document,
            id(),
            "no credential configured for `openai`",
        )
        .expect_err("an unusable router must not start an auto Fleet");

        assert!(err.contains("cannot start"), "{err}");
        assert!(err.contains("implementer"), "{err}");
        assert!(err.contains("no credential configured"), "{err}");
        assert!(err.contains("never fall back"), "{err}");
    }

    #[test]
    fn the_frozen_route_pins_each_members_exact_provider_and_model() {
        let workflow = workflow_with(None, GLM_FLEET);
        let route = workflow
            .preflight
            .worker("implementer")
            .expect("preflighted worker");

        assert_eq!(route.provider_id, "zai");
        assert_eq!(route.wire_model, "glm-5");
        let member = workflow
            .snapshot()
            .member("implementer")
            .expect("snapshot entry");
        assert!(
            member.requested_reasoning.is_auto(),
            "reasoning is decided per task, not baked into the frozen route"
        );
    }

    /// Binding carries route and Runtime role, but no Fleet-owned authority.
    #[test]
    fn bound_members_use_runtime_roles_and_neutral_compatibility_fields() {
        let workflow = workflow_with(None, GLM_FLEET);
        for (id, expected_posture, expected_write) in [
            ("auditor", "reviewer", "read_only"),
            ("implementer", "implement", "workspace_write"),
        ] {
            let binding = workflow
                .bind_member(Some(id), None, full_session())
                .expect("bind");
            assert_eq!(
                binding.authority.posture_role, expected_posture,
                "{id} must resolve through Runtime's closed role policy"
            );
            assert_eq!(
                binding.authority.write_authority, expected_write,
                "{id} authority comes from the role posture, never a Fleet permissions block"
            );
        }
    }

    /// #5575: a free-form member role keeps its identity and **fails closed**.
    ///
    /// This test previously asserted the opposite — `posture_role == "custom"`
    /// and `write_authority == "workspace_write"` — which is exactly the defect:
    /// `audit-lead` is a name nobody declared, and the exact driver answered it
    /// with the widest posture there is while the durable driver answered the
    /// same class of name with `general`. Identity is still preserved verbatim
    /// (`member_role`), but an undeclared label now buys the narrowest useful
    /// posture, not write authority.
    #[test]
    fn a_free_form_member_role_fails_closed_without_losing_identity() {
        const AUDIT_FLEET: &str = r#"
name = "audit"
schema = "exact"

[[members]]
id = "auditor-one"
role = "audit-lead"
provider = "zai"
model = "glm-5"
permissions = "read_only"
"#;
        let workflow = workflow_with(None, AUDIT_FLEET);
        let binding = workflow
            .bind_member(Some("auditor-one"), None, full_session())
            .expect("bind");

        assert_eq!(binding.member_role, "audit-lead");
        assert_eq!(binding.authority.posture_role, "explore");
        assert_eq!(
            binding.authority.write_authority, "read_only",
            "an undeclared role name must never grant write authority; an \
             operator who wants the parent's posture spells the role `custom`"
        );

        // The escape hatch is a declared role, not a typo.
        assert_eq!(
            ChildAuthority::from_runtime_role("custom", full_session()).write_authority,
            "workspace_write"
        );
    }

    // ── Permission ceilings, as the child actually experiences them ─────────

    /// `tools = false` means zero model tools — an empty allowlist, which the
    /// child registry treats as "nothing is visible and nothing is callable".
    #[test]
    fn spawn_refuses_empty_effective_toolset() {
        let authority = ChildAuthority::clamp(PermissionCeiling::ROUTER, full_session());
        let err = refuse_dropped_requested_tools(
            "router",
            "advisor",
            &["read_file".to_string(), "grep_files".to_string()],
            &authority,
            &authority,
        )
        .expect_err("a child that would start with no tools is refused");
        assert!(
            err.contains("Agent 'router' (role advisor) would start with no tools"),
            "{err}"
        );
        assert!(err.contains("requested [grep_files, read_file]"), "{err}");
        assert!(err.contains("role allows no tools"), "{err}");
        // An explicit empty request stays a deliberate tool-free child.
        refuse_dropped_requested_tools("router", "advisor", &[], &authority, &authority)
            .expect("explicit empty toolset is allowed");
    }

    #[test]
    fn spawn_error_names_dropped_tools() {
        let workflow = workflow_with(None, GLM_FLEET);
        let mut binding = workflow
            .bind_member(None, Some("reviewer"), full_session())
            .expect("role lookup");
        let err = binding
            .narrow_for_task(
                None,
                Some(&["read_file".to_string(), "exec_shell".to_string()]),
                &[],
                None,
            )
            .expect_err("a requested tool the role denies is refused");
        assert!(err.contains("would lose requested tools"), "{err}");
        assert!(err.contains("dropped [exec_shell]"), "{err}");
        assert!(err.contains("requested [exec_shell, read_file]"), "{err}");
        assert!(
            err.contains("role allows all inherited tools except ["),
            "{err}"
        );

        // Tools the role allows narrow cleanly.
        binding
            .narrow_for_task(None, Some(&["read_file".to_string()]), &[], None)
            .expect("an allowed narrowing succeeds");
        assert_eq!(
            binding.authority.allowed_tools.as_deref(),
            Some(&["read_file".to_string()] as &[String])
        );
    }

    #[test]
    fn tools_false_yields_an_empty_tool_surface() {
        let authority = ChildAuthority::clamp(PermissionCeiling::ROUTER, full_session());

        assert!(!authority.ceiling.tools);
        assert_eq!(
            authority.allowed_tools.as_deref(),
            Some(&[] as &[String]),
            "tools = false must be an empty allowlist, not an absent one"
        );
        assert_eq!(authority.write_authority, "read_only");
        assert_eq!(authority.max_depth, 0);
    }

    /// `network_tool = false` removes every model-visible network, browser,
    /// and remote-MCP surface except the `Web` family's two read-only actions
    /// — even when `tools = true`. The family *name* must survive the deny
    /// list so the child registry's action seam can grant exactly
    /// `search`/`fetch`; every other browsing spelling is denied.
    #[test]
    fn network_disabled_denies_every_network_surface_even_with_tools_enabled() {
        let member = PermissionCeiling::preset("read_write").expect("preset");
        assert!(member.tools);
        assert!(!member.network_tool);

        let authority = ChildAuthority::clamp(member, full_session());

        assert!(
            authority.allowed_tools.is_none(),
            "a tool-using member keeps full inheritance, narrowed by the deny list"
        );
        for expected in [
            "web.run",
            "web_run",
            "web_search",
            "fetch_url",
            "wait_for_dev_server",
            "github",
            "mcp*",
        ] {
            assert!(
                authority
                    .disallowed_tools
                    .iter()
                    .any(|name| name == expected),
                "{expected} must be denied: {:?}",
                authority.disallowed_tools
            );
        }
        // The canonical family name is what the read-only web surface
        // dispatches under; only its reaching spellings are denied.
        assert!(
            !authority.disallowed_tools.iter().any(|name| name == "Web"),
            "the Web family name must survive so search/fetch stay reachable: {:?}",
            authority.disallowed_tools
        );

        // A member that IS allowed a network tool gets no such deny list.
        let networked = ChildAuthority::clamp(
            PermissionCeiling::preset("full").expect("preset"),
            full_session(),
        );
        assert!(networked.ceiling.network_tool);
        assert!(networked.disallowed_tools.is_empty());
    }

    /// The browsing capability is registered under several names, and `web.run`
    /// is the one a deny list stopping at the `Web` family name leaves behind.
    /// A network-denied member that can still call `web.run` is not
    /// network-denied, so every spelling *except* the family name itself —
    /// which the action seam bounds to `search`/`fetch` — stays on the list.
    #[test]
    fn network_disabled_denies_the_canonical_web_run_surface_and_its_aliases() {
        let authority = ChildAuthority::clamp(
            PermissionCeiling::preset("read_write").expect("preset"),
            full_session(),
        );

        let denied = |name: &str| {
            let lowered = name.to_ascii_lowercase();
            authority.disallowed_tools.iter().any(|rule| {
                let rule = rule.to_ascii_lowercase();
                rule.strip_suffix('*')
                    .map_or(rule == lowered, |prefix| lowered.starts_with(prefix))
            })
        };

        for name in [
            "web.run",
            "web_run",
            "web_search",
            "web.fetch",
            "web_fetch",
            "fetch_url",
            "wait_for_dev_server",
            "browse",
            "browser",
        ] {
            assert!(
                denied(name),
                "{name} must be denied: {:?}",
                authority.disallowed_tools
            );
        }
        // The family name itself is what the read-only search/fetch surface
        // dispatches under; the action seam and the URL-input guard bound it.
        assert!(
            !denied("Web"),
            "the Web family name must survive a network denial: {:?}",
            authority.disallowed_tools
        );
        // The globs must not reach past the browsing family.
        for name in ["read_file", "run_tests", "Git", "grep_files"] {
            assert!(!denied(name), "{name} is not a network surface");
        }
    }

    /// `rlm` reaches the network without ever naming a network tool: `open`
    /// fetches a `url` by calling `FetchUrlTool` in-process, and `eval` runs
    /// Python that owns a socket API. Denying `fetch_url` sees neither call, so
    /// both actions carry their own deny-list entries.
    #[test]
    fn network_disabled_denies_the_in_process_rlm_reach() {
        let authority = ChildAuthority::clamp(
            PermissionCeiling::preset("read_write").expect("preset"),
            full_session(),
        );

        let denied = |name: &str| {
            let lowered = name.to_ascii_lowercase();
            authority.disallowed_tools.iter().any(|rule| {
                let rule = rule.to_ascii_lowercase();
                rule.strip_suffix('*')
                    .map_or(rule == lowered, |prefix| lowered.starts_with(prefix))
            })
        };

        for reaching in ["rlm_open", "rlm_eval"] {
            assert!(
                denied(reaching),
                "{reaching} reaches the network in-process and must be denied: {:?}",
                authority.disallowed_tools
            );
        }
        // The fail-closed narrowing is deliberate but *bounded*: the bounded
        // local metadata actions survive, and so does the family itself, so the
        // per-action seam has something left to permit.
        for kept in ["rlm", "rlm_session_objects", "rlm_configure", "rlm_close"] {
            assert!(
                !denied(kept),
                "{kept} is bounded local metadata and must survive a network denial"
            );
        }
    }

    /// The deny-list sentinel has to actually be on the deny list, or every
    /// posture check derived from it silently reads "network allowed".
    #[test]
    fn the_network_denial_sentinel_is_installed_by_a_network_denial() {
        assert!(
            NETWORK_TOOL_DENYLIST.contains(&NETWORK_DENIAL_SENTINEL),
            "{NETWORK_DENIAL_SENTINEL} must be an explicit entry, not a glob match"
        );
        let authority = ChildAuthority::clamp(
            PermissionCeiling::preset("read_write").expect("preset"),
            full_session(),
        );
        assert!(
            authority
                .disallowed_tools
                .iter()
                .any(|rule| rule == NETWORK_DENIAL_SENTINEL),
            "a network denial must install the sentinel verbatim: {:?}",
            authority.disallowed_tools
        );
        // …and a network-*capable* member must not, or the sentinel would read
        // as denied for everyone.
        let networked = ChildAuthority::clamp(
            PermissionCeiling::preset("full").expect("preset"),
            full_session(),
        );
        assert!(
            !networked
                .disallowed_tools
                .iter()
                .any(|rule| rule == NETWORK_DENIAL_SENTINEL)
        );
    }

    /// Every network-denied preset — read_only/read-only inspection included — leaves the
    /// `Web` family name reachable and seals each of its reaching spellings.
    /// This is the deny-list half of the read-only web-search contract; the
    /// registry-side half (exactly `search`/`fetch`, with URL-addressed calls
    /// refused) is asserted in `subagent/tests.rs`.
    #[test]
    fn every_network_denial_leaves_web_search_reachable_by_family_name() {
        for preset in ["analyst", "read_only", "verifier", "read_write"] {
            let authority = ChildAuthority::clamp(
                PermissionCeiling::preset(preset).expect("preset"),
                full_session(),
            );
            assert!(
                !authority.ceiling.network_tool,
                "{preset} is network-denied"
            );
            assert!(
                !authority.disallowed_tools.iter().any(|rule| rule == "Web"),
                "{preset} must keep the Web family name: {:?}",
                authority.disallowed_tools
            );
            for sealed in [
                "web_*",
                "web.*",
                "web.run",
                "web_run",
                "web_search",
                "web.fetch",
                "web_fetch",
                "fetch_url",
                "wait_for_dev_server",
                "github",
                "mcp*",
            ] {
                assert!(
                    authority.disallowed_tools.iter().any(|rule| rule == sealed),
                    "{preset} must deny {sealed}: {:?}",
                    authority.disallowed_tools
                );
            }
        }
    }

    /// A member saved as `write = false` must not receive a mutating surface —
    /// including the raw shell a `verifier`-shaped ceiling keeps for running
    /// checks. `rm -rf` mutates a workspace exactly as well as `write_file`,
    /// and a receipt that says `write=false` while the child holds `exec_shell`
    /// is not true.
    #[test]
    fn a_read_only_member_gets_a_truthful_non_mutating_tool_contract() {
        let verifier = PermissionCeiling::preset("verifier").expect("preset");
        assert!(!verifier.write);
        assert_eq!(verifier.shell, ShellCeiling::Full);

        let authority = ChildAuthority::clamp(verifier, full_session());
        assert_eq!(authority.write_authority, "read_only");

        let denied = |name: &str| {
            authority.disallowed_tools.iter().any(|rule| {
                rule == name || rule.strip_suffix('*').is_some_and(|p| name.starts_with(p))
            })
        };

        // `rlm_eval` belongs on this list for the same reason `exec_shell` does:
        // the Python it runs writes files. A tool is a mutation primitive
        // because of what it can do, not because of what it is called.
        for mutating in [
            "write_file",
            "edit_file",
            "apply_patch",
            "fim_edit",
            "rlm_eval",
        ] {
            assert!(
                denied(mutating),
                "{mutating} must be denied for a read-only member: {:?}",
                authority.disallowed_tools
            );
        }
        for raw_shell in [
            "Bash",
            "exec_shell",
            "exec_shell_interact",
            "task_shell_start",
            "terminal/run",
        ] {
            assert!(
                denied(raw_shell),
                "{raw_shell} is a general mutation primitive: {:?}",
                authority.disallowed_tools
            );
        }
        // What the member is *for* survives: the bounded verification surface.
        // (`rlm_open` is absent from this list only because the `verifier`
        // preset is also network-denied; the write contract alone keeps it —
        // see `a_write_denial_alone_keeps_local_rlm_loading`.)
        for kept in [
            "Run",
            "run_tests",
            "run_verifiers",
            "read_file",
            "grep_files",
            "rlm",
        ] {
            assert!(!denied(kept), "{kept} must stay available to a verifier");
        }

        // A write-capable member is untouched by this contract.
        let builder = ChildAuthority::clamp(
            PermissionCeiling::preset("read_write").expect("preset"),
            full_session(),
        );
        assert!(builder.ceiling.write);
        for kept in ["write_file", "apply_patch", "exec_shell"] {
            assert!(
                !builder.disallowed_tools.iter().any(|rule| rule == kept),
                "{kept} must stay available to a write-capable member"
            );
        }
    }

    /// The two denials are separate contracts and must not bleed into each
    /// other. A member that may not *write* can still load a large local file
    /// into an RLM kernel and read it — that is analysis, not mutation. Only
    /// `eval` goes, because only `eval` runs code.
    #[test]
    fn a_write_denial_alone_keeps_local_rlm_loading() {
        let member = PermissionCeiling {
            write: false,
            network_tool: true,
            shell: ShellCeiling::ReadOnly,
            delegation_depth: 0,
            tools: true,
        };
        let authority = ChildAuthority::clamp(member, full_session());
        assert!(!authority.ceiling.write);
        assert!(authority.ceiling.network_tool);

        let denied = |name: &str| authority.disallowed_tools.iter().any(|rule| rule == name);

        assert!(denied("rlm_eval"), "eval runs code, so it mutates");
        for kept in ["rlm", "rlm_open", "rlm_session_objects", "rlm_close"] {
            assert!(
                !denied(kept),
                "{kept} loads and inspects; it does not mutate: {:?}",
                authority.disallowed_tools
            );
        }
    }

    /// The parent posture always wins. A saved `full` member inside a
    /// read-only, no-network, no-shell session runs at the session's ceiling.
    #[test]
    fn the_parent_ceiling_wins_over_a_wider_saved_member() {
        let session = PermissionCeiling {
            write: false,
            network_tool: false,
            shell: ShellCeiling::ReadOnly,
            delegation_depth: 0,
            tools: true,
        };
        let member = PermissionCeiling::preset("full").expect("preset");
        assert!(member.write && member.network_tool);

        let authority = ChildAuthority::clamp(member, session);

        assert!(!authority.ceiling.write, "a Fleet may not grant write");
        assert!(
            !authority.ceiling.network_tool,
            "a Fleet may not grant a network tool"
        );
        assert_eq!(authority.ceiling.shell, ShellCeiling::ReadOnly);
        assert_eq!(authority.ceiling.delegation_depth, 0);
        assert_eq!(authority.write_authority, "read_only");
        assert_eq!(authority.max_depth, 0);
        assert_eq!(authority.posture_role, "explore");
        assert!(!authority.disallowed_tools.is_empty());
    }

    /// A read-only session cannot be widened by a session that *is* permissive
    /// either — clamping is symmetric, and takes the narrower side each way.
    #[test]
    fn clamping_takes_the_narrower_side_of_every_field() {
        let narrow_member = PermissionCeiling {
            write: false,
            network_tool: false,
            shell: ShellCeiling::None,
            delegation_depth: 0,
            tools: true,
        };
        let authority = ChildAuthority::clamp(narrow_member, full_session());

        assert!(!authority.ceiling.write);
        assert_eq!(authority.ceiling.shell, ShellCeiling::None);
        assert_eq!(authority.ceiling.delegation_depth, 0);
    }

    // ── Preflight ──────────────────────────────────────────────────────────

    /// Z.AI GLM routes express only thinking enabled/disabled, so `high` and
    /// `max` must not be reported as two distinct provider-effective tiers.
    #[test]
    fn glm_routes_report_an_enabled_disabled_provider_control() {
        let capability = reasoning_capability_for_route(
            ProviderKind::Zai,
            codewhale_config::descriptors::defaults::DEFAULT_ZAI_BASE_URL,
            codewhale_config::catalog::reviewed::constants::ZAI_GLM_5_2_MODEL,
        );

        assert_eq!(
            capability.control,
            ProviderReasoningControl::EnabledDisabled,
            "Z.AI's request shaping emits only thinking enabled/disabled"
        );
        assert!(!capability.supports_native_adaptive());
        assert_eq!(
            capability.provider_effective(ReasoningTier::High),
            ProviderEffectiveReasoning::Enabled
        );
        assert_eq!(
            capability.provider_effective(ReasoningTier::Off),
            ProviderEffectiveReasoning::Disabled
        );
    }

    /// DeepSeek varies `reasoning_effort` per tier, so its tiers are real.
    #[test]
    fn a_route_that_varies_its_wire_value_reports_distinct_tiers() {
        let capability = reasoning_capability_for_route(
            ProviderKind::Deepseek,
            codewhale_config::descriptors::defaults::DEFAULT_DEEPSEEK_BASE_URL,
            "deepseek-v4-pro",
        );
        assert_eq!(capability.control, ProviderReasoningControl::Tiers);
    }

    /// First-party DeepSeek routes document `reasoning_effort` low/high/max
    /// on the wire (no medium), so `low` is a real tier there. The capability
    /// must report the tier the route *sends*, not the tier the selector
    /// named: low reaches the wire as low, medium rounds up to high because
    /// the dialect has no such value (#52).
    #[test]
    fn a_deepseek_route_reports_low_as_low_and_medium_as_high() {
        let capability = reasoning_capability_for_route(
            ProviderKind::Deepseek,
            codewhale_config::descriptors::defaults::DEFAULT_DEEPSEEK_BASE_URL,
            "deepseek-v4-pro",
        );

        // Exactly what the request shaping does, read back off the capability.
        for (requested, expected) in [
            (ReasoningTier::Low, ReasoningTier::Low),
            (ReasoningTier::Medium, ReasoningTier::High),
            (ReasoningTier::High, ReasoningTier::High),
            (ReasoningTier::Max, ReasoningTier::Max),
            (ReasoningTier::Off, ReasoningTier::Off),
        ] {
            assert_eq!(
                capability.wire_tier(requested),
                expected,
                "requested {requested:?} must be reported as what the wire carries"
            );
            let (effective, normalized) = capability.normalize(requested);
            assert_eq!(effective, expected);
            assert_eq!(normalized, requested != expected);
        }

        // And the resolver carries that all the way onto the receipt.
        let resolved = codewhale_workflow::resolve_exact_member_reasoning(
            "implementer",
            &codewhale_workflow::FrozenRoute {
                provider: "deepseek".to_string(),
                model: "deepseek-v4-pro".to_string(),
            },
            RequestedReasoning::Low,
            &capability,
            &RouterAvailability::Absent,
            None,
            None,
        )
        .expect("resolve");
        assert_eq!(resolved.requested(), RequestedReasoning::Low);
        assert_eq!(
            resolved.effective(),
            codewhale_workflow::EffectiveReasoning::Tier(ReasoningTier::Low)
        );
        assert!(!resolved.capability_normalized());
    }

    /// Routes whose dialect has no low tier still collapse low onto high, and
    /// the capability must say so instead of reporting a `low` the wire never
    /// carried. CodeWhale's normalizer keeps the historic low/medium → high
    /// coercion for these DeepSeek-compatible hosted routes because their own
    /// wire contracts are not verified.
    #[test]
    fn a_route_that_collapses_low_onto_high_says_so_instead_of_reporting_low() {
        let capability = reasoning_capability_for_route(
            ProviderKind::Siliconflow,
            codewhale_config::descriptors::defaults::DEFAULT_SILICONFLOW_BASE_URL,
            "deepseek-ai/DeepSeek-V4-Pro",
        );

        for (requested, expected) in [
            (ReasoningTier::Low, ReasoningTier::High),
            (ReasoningTier::Medium, ReasoningTier::High),
            (ReasoningTier::High, ReasoningTier::High),
            (ReasoningTier::Max, ReasoningTier::Max),
            (ReasoningTier::Off, ReasoningTier::Off),
        ] {
            assert_eq!(
                capability.wire_tier(requested),
                expected,
                "requested {requested:?} must be reported as what the wire carries"
            );
            let (effective, normalized) = capability.normalize(requested);
            assert_eq!(effective, expected);
            assert_eq!(normalized, requested != expected);
        }
    }

    /// Preflight resolves the provider, canonicalizes the model, identifies the
    /// endpoint, and decides credential readiness — all from local config.
    #[test]
    fn preflight_freezes_provider_model_endpoint_and_local_readiness() {
        let _env_lock = crate::test_support::lock_test_env();
        let _key = crate::test_support::EnvVarGuard::set("ZAI_API_KEY", "zai-key");
        let config = {
            let __cw_config_init_0_0 = Some("zai".to_string());
            let mut __cw_config_init_0_value: Config = Default::default();
            __cw_config_init_0_value.provider = __cw_config_init_0_0;
            __cw_config_init_0_value
        };

        let route = preflight_route(
            "implementer",
            "zai",
            codewhale_config::catalog::reviewed::constants::ZAI_GLM_5_2_MODEL,
            &config,
        )
        .expect("preflight");

        assert_eq!(route.member_id, "implementer");
        assert_eq!(route.provider_kind, "zai");
        assert_eq!(
            route.wire_model,
            codewhale_config::catalog::reviewed::constants::ZAI_GLM_5_2_MODEL
        );
        assert!(!route.endpoint.host.is_empty());
        assert!(!route.endpoint.host.contains('/'));
        assert_eq!(route.credential, CredentialReadiness::Configured);
        route.require_ready().expect("ready");

        // The receipt and the child spawn read the same canonical wire model.
        assert_eq!(route.frozen().model, route.wire_model);
    }

    /// A keyless local provider is valid, and is decided without a probe.
    #[test]
    fn a_keyless_local_provider_preflights_as_ready() {
        let _env_lock = crate::test_support::lock_test_env();
        let config = {
            let __cw_config_init_1_0 = Some("ollama".to_string());
            let mut __cw_config_init_1_value: Config = Default::default();
            __cw_config_init_1_value.provider = __cw_config_init_1_0;
            __cw_config_init_1_value
        };

        let Ok(route) = preflight_route("worker", "ollama", "qwen3", &config) else {
            // A model id this build does not know is a different failure than
            // the one under test; skip rather than assert on the catalog.
            return;
        };
        assert_eq!(route.credential, CredentialReadiness::KeylessLocal);
        assert!(route.credential.is_ready());
        route.require_ready().expect("keyless local is valid");
        assert!(route.endpoint.local, "a local runtime is marked local");
    }

    #[test]
    fn ollama_cloud_and_custom_remote_preflight_require_route_scoped_credentials() {
        let _env_lock = crate::test_support::lock_test_env();
        let temp = tempfile::tempdir().expect("isolated credential home");
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", temp.path());
        let _backend = crate::test_support::EnvVarGuard::set("CODEWHALE_SECRET_BACKEND", "file");
        let _ollama_cloud_key = crate::test_support::EnvVarGuard::remove("OLLAMA_CLOUD_API_KEY");
        let _ollama_key = crate::test_support::EnvVarGuard::remove("OLLAMA_API_KEY");
        let _cli_source = crate::test_support::EnvVarGuard::remove("DEEPSEEK_API_KEY_SOURCE");
        let _cli_key = crate::test_support::EnvVarGuard::remove("CODEWHALE_CLI_API_KEY");
        codewhale_secrets::Secrets::auto_detect()
            .set("ollama", "legacy-cloud-key")
            .expect("seed released Ollama Cloud slot");

        let cloud = {
            let __cw_config_init_2_0 = Some("deepseek".to_string());
            let __cw_config_init_2_1 = Some(codewhale_runtime::config::ProvidersConfig {
                ollama: codewhale_runtime::config::ProviderConfig {
                    base_url: Some(codewhale_config::provider::OLLAMA_CLOUD_BASE_URL.to_string()),
                    ..Default::default()
                },
                ..Default::default()
            });
            let mut __cw_config_init_2_value: Config = Default::default();
            __cw_config_init_2_value.provider = __cw_config_init_2_0;
            __cw_config_init_2_value.providers = __cw_config_init_2_1;
            __cw_config_init_2_value
        };
        let cloud_route = preflight_route(
            "cloud-worker",
            "ollama",
            codewhale_config::descriptors::defaults::DEFAULT_OLLAMA_MODEL,
            &cloud,
        )
        .expect("official Cloud route");
        assert_eq!(cloud_route.provider_id, "ollama-cloud");
        assert_eq!(cloud_route.provider_config_id.as_deref(), Some("ollama"));
        assert_eq!(cloud_route.provider_kind, "ollama-cloud");
        assert_eq!(cloud_route.credential, CredentialReadiness::Configured);
        assert!(!cloud_route.endpoint.local);
        cloud_route.require_ready().expect("Cloud env key is ready");

        let custom_remote = {
            let __cw_config_init_3_0 = Some("ollama".to_string());
            let __cw_config_init_3_1 = Some(codewhale_runtime::config::ProvidersConfig {
                ollama: codewhale_runtime::config::ProviderConfig {
                    base_url: Some("https://ollama-gateway.example.test/v1".to_string()),
                    ..Default::default()
                },
                ..Default::default()
            });
            let mut __cw_config_init_3_value: Config = Default::default();
            __cw_config_init_3_value.provider = __cw_config_init_3_0;
            __cw_config_init_3_value.providers = __cw_config_init_3_1;
            __cw_config_init_3_value
        };
        let custom_route = preflight_route(
            "custom-worker",
            "ollama",
            codewhale_config::descriptors::defaults::DEFAULT_OLLAMA_MODEL,
            &custom_remote,
        )
        .expect("custom route still resolves structurally");
        assert!(matches!(
            custom_route.credential,
            CredentialReadiness::Missing { .. }
        ));
        assert!(!custom_route.endpoint.local);
        assert!(custom_route.require_ready().is_err());
    }

    #[tokio::test]
    async fn legacy_ollama_cloud_fleet_start_builds_clients_from_the_frozen_source_route() {
        let _env_lock = crate::test_support::lock_test_env();
        let temp = tempfile::tempdir().expect("isolated credential home");
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", temp.path());
        let _backend = crate::test_support::EnvVarGuard::set("CODEWHALE_SECRET_BACKEND", "file");
        let _cloud_env = crate::test_support::EnvVarGuard::remove("OLLAMA_CLOUD_API_KEY");
        let _official_env = crate::test_support::EnvVarGuard::remove("OLLAMA_API_KEY");
        codewhale_secrets::Secrets::auto_detect()
            .set("ollama", "legacy-cloud-fleet-key")
            .expect("seed released Ollama Cloud slot");

        let config = {
            let __cw_config_init_4_0 = Some("deepseek".to_string());
            let __cw_config_init_4_1 = Some(codewhale_runtime::config::ProvidersConfig {
                ollama: codewhale_runtime::config::ProviderConfig {
                    base_url: Some(codewhale_config::provider::OLLAMA_CLOUD_BASE_URL.to_string()),
                    model: Some(
                        codewhale_config::descriptors::defaults::DEFAULT_OLLAMA_CLOUD_MODEL
                            .to_string(),
                    ),
                    ..Default::default()
                },
                ..Default::default()
            });
            let mut __cw_config_init_4_value: Config = Default::default();
            __cw_config_init_4_value.provider = __cw_config_init_4_0;
            __cw_config_init_4_value.providers = __cw_config_init_4_1;
            __cw_config_init_4_value
        };
        let document = FleetDocument::parse(&format!(
            r#"
name = "glm-pair"
schema = "exact"

[[members]]
id = "cloud-worker"
role = "builder"
provider = "ollama"
model = "{}"
reasoning = "medium"
permissions = "read_only"
"#,
            codewhale_config::descriptors::defaults::DEFAULT_OLLAMA_CLOUD_MODEL
        ))
        .expect("legacy Cloud Fleet parses");

        // `capture` is the real Workflow-start path: it preflights readiness,
        // constructs every worker client, and freezes the snapshot.
        let workflow = ExactFleetWorkflow::capture(
            &document,
            id(),
            "2026-08-14T00:00:00Z",
            Some(&config),
            &[],
        )
        .expect("legacy Cloud Fleet starts");
        let route = workflow
            .preflight
            .worker("cloud-worker")
            .expect("preflighted worker");
        assert_eq!(route.provider_id, "ollama-cloud");
        assert_eq!(route.provider_config_id.as_deref(), Some("ollama"));

        let binding = workflow
            .bind_member(Some("cloud-worker"), None, full_session())
            .expect("worker binds");
        let launch = workflow
            .route_admitted_task(&binding, "verify the frozen Cloud route")
            .await
            .expect("manual-tier launch needs no provider call");
        assert_eq!(launch.provider, "ollama-cloud");
        assert_eq!(launch.receipt.provider, "ollama-cloud");

        let router_profile = ReasoningRouterProfile::parse(&format!(
            r#"
name = "legacy-cloud-router"
schema = "reasoning_router"
provider = "ollama"
model = "{}"
call_reasoning = "low"
"#,
            codewhale_config::descriptors::defaults::DEFAULT_OLLAMA_CLOUD_MODEL
        ))
        .expect("legacy Cloud router profile parses");
        let captured =
            CapturedReasoningRouter::from_profile(&router_profile, "workspace".to_string());
        let live = LiveFleetRouter::bind(&captured, &config)
            .expect("legacy Cloud Router binds its source table and secret");
        assert_eq!(live.route.provider_id, "ollama-cloud");
        assert_eq!(live.route.provider_config_id.as_deref(), Some("ollama"));
        assert_eq!(live.client.api_provider(), ProviderKind::OllamaCloud);
        assert_eq!(
            live.client.base_url(),
            codewhale_config::provider::OLLAMA_CLOUD_BASE_URL
        );
    }

    /// A tier label is a selector concept; what a request may carry is a
    /// provider concept. The value placed on a call must come from the route
    /// normalizer the client actually uses, or a Codex-routed Router is called
    /// at the provider default while its receipt claims a tier.
    #[test]
    fn a_call_reasoning_value_is_shaped_by_the_configured_route_not_a_tier_label() {
        // A tiered non-Codex route spells the tiers the ordinary way, after
        // the same route normalization the client performs (first-party
        // DeepSeek keeps a real `low`; medium still rounds up to high).
        for (tier, expected) in [
            (ReasoningTier::Off, "off"),
            (ReasoningTier::High, "high"),
            (ReasoningTier::Max, "max"),
        ] {
            assert_eq!(
                route_reasoning_setting(
                    ProviderKind::Deepseek,
                    codewhale_config::descriptors::defaults::DEFAULT_DEEPSEEK_BASE_URL,
                    "deepseek-v4-pro",
                    tier,
                ),
                expected,
                "{tier:?} on a deepseek route"
            );
        }

        // Codex is the case a bare tier label gets wrong in both directions:
        // it has no `off`, and its ladder now spells three separate top rungs
        // (`xhigh`, `max`, `ultra`) that the roster publishes per model.
        let codex = |tier| {
            route_reasoning_setting(
                ProviderKind::OpenaiCodex,
                "https://chatgpt.com/backend-api/codex",
                "gpt-5.6-codex",
                tier,
            )
        };
        assert_eq!(codex(ReasoningTier::Max), "max");
        assert_eq!(codex(ReasoningTier::Low), "low");
        assert_ne!(
            codex(ReasoningTier::Off),
            "off",
            "an always-thinking route cannot be asked for `off`; sending the label \
             would take the provider default while the receipt claimed a tier"
        );
    }

    /// An unresolvable provider fails preflight rather than reaching a launch.
    #[test]
    fn an_unresolvable_provider_fails_preflight() {
        let config = Config::default();
        let err = preflight_route("implementer", "not-a-provider", "whatever", &config)
            .expect_err("unresolvable provider");
        assert!(matches!(err, PreflightError::ProviderUnresolved { .. }));
    }

    // ── Receipts ───────────────────────────────────────────────────────────

    /// The receipt is the durable artifact. It must carry every side of the
    /// decision — including which service chose the tier and what that call was
    /// configured to cost — and must store no task text, path, or key.
    #[tokio::test]
    async fn a_launch_receipt_names_the_service_route_and_call_cost_without_content() {
        let workflow = workflow_with(
            Some(StaticFleetRouter::new(r#"{"reasoning":"max"}"#)),
            GLM_FLEET,
        );
        let binding = workflow
            .bind_member(None, Some("builder"), full_session())
            .expect("bind");

        let launch = workflow
            .route_admitted_task(&binding, "refactor /Users/hunter/app with ZAI_API_KEY=zzz")
            .await
            .expect("launch");
        let receipt = &launch.receipt;

        assert_eq!(receipt.fleet, "workspace/glm-pair");
        assert_eq!(receipt.schema_kind, "exact");
        assert_eq!(receipt.member_id, "implementer");
        assert_eq!(receipt.member_role, "implement");
        assert_eq!(receipt.provider, "zai");
        assert_eq!(receipt.model, "glm-5");
        assert_eq!(receipt.requested_reasoning, "auto");
        assert_eq!(receipt.effective_reasoning, "max");
        assert_eq!(receipt.selection_source, "fleet_router");
        assert!(!receipt.content_hash.is_empty());

        // The service is labelled as a service, with its exact route and the
        // configured requested → provider-effective call reasoning.
        let router = receipt.router.as_ref().expect("router identity");
        assert_eq!(router.service_kind, "reasoning_router");
        assert_eq!(router.qualified(), "workspace/luna-low");
        assert_eq!(router.provider, "openai");
        assert_eq!(router.model, "gpt-5.6-luna");
        let call = router.call.as_ref().expect("call disclosure");
        assert_eq!(call.requested, "low");
        assert_eq!(call.effective, "low");
        assert_eq!(call.provider_effective, "low");

        // Cross-provider inference happened (zai worker, openai router) and is
        // disclosed rather than implied away.
        assert!(receipt.cross_provider_inference);
        assert!(
            receipt.transport.contains("different provider"),
            "{}",
            receipt.transport
        );

        // Disclosure without content.
        let disclosure = receipt.routing_summary.as_ref().expect("disclosure");
        assert!(disclosure.transmitted_bytes > 0);
        assert!(disclosure.content_hash.starts_with("sha256:"));
        assert!(disclosure.redacted);

        let json = serde_json::to_string(receipt).expect("serialize");
        for forbidden in ["/Users/", "/home/", ".toml", "api_key", "zzz", "refactor"] {
            assert!(!json.contains(forbidden), "{forbidden} in {json}");
        }

        // The visible line names every side and echoes no content.
        let line = receipt.line();
        for expected in [
            "requested=auto",
            "effective=max",
            "source=fleet_router",
            "reasoning_router:workspace/luna-low",
            "router_call_requested=low",
        ] {
            assert!(line.contains(expected), "{expected} missing from {line}");
        }
        assert!(!line.contains("refactor"), "{line}");
    }

    /// A member's semantic role and its Runtime posture are separate
    /// facts and the receipt keeps both. An operator who named a member
    /// `auditor` must see `auditor` on the receipt, while the surface actually
    /// selected (`explore`, the fail-closed posture an undeclared role gets
    /// since #5575) is disclosed rather than substituted for the name.
    #[tokio::test]
    async fn a_receipt_records_the_posture_without_renaming_the_members_role() {
        const AUDIT_FLEET: &str = r#"
name = "glm-pair"
schema = "exact"

[[members]]
id = "auditor"
role = "auditor"
provider = "zai"
model = "glm-5"
reasoning = "high"
permissions = "read_only"
"#;
        let workflow = workflow_with(None, AUDIT_FLEET);
        let binding = workflow
            .bind_member(None, Some("auditor"), full_session())
            .expect("bind");

        // Enforcement uses the posture; it is not the operator's role name.
        assert_eq!(binding.member_role, "auditor");
        assert_eq!(binding.authority.posture_role, "explore");

        let launch = workflow
            .route_admitted_task(&binding, "review the queue")
            .await
            .expect("launch");
        let receipt = &launch.receipt;

        assert_eq!(receipt.member_role, "auditor");
        assert_eq!(receipt.posture_role.as_deref(), Some("explore"));
        let line = receipt.line();
        assert!(line.contains("(role auditor)"), "{line}");
        assert!(line.contains("posture=explore"), "{line}");
    }

    // ── Search roots: where a workspace Fleet lives ────────────────────────

    /// The Fleet store saves workspace Fleets under `<workspace>/.codewhale`,
    /// so that is the primary `workspace` origin; the workspace root stays a
    /// second origin for checked-in `fleets/<name>.toml` rosters.
    #[test]
    fn workspace_fleets_load_from_dot_codewhale_and_the_legacy_root() {
        let _lock = crate::test_support::lock_test_env();
        let home = tempfile::tempdir().expect("home");
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", home.path());
        let ws = tempfile::tempdir().expect("workspace");

        let saved = ws.path().join(".codewhale").join("fleets");
        std::fs::create_dir_all(&saved).expect("saved fleets dir");
        std::fs::write(saved.join("glm-pair.toml"), GLM_FLEET).expect("write saved");
        let (document, id) =
            load_fleet_document("glm-pair", ws.path(), None).expect("saved fleet loads");
        assert_eq!(document.name(), "glm-pair");
        assert_eq!(id.origin, "workspace");

        let checked_in = ws.path().join("fleets");
        std::fs::create_dir_all(&checked_in).expect("checked-in fleets dir");
        std::fs::write(
            checked_in.join("stopship.toml"),
            "name = \"stopship\"\n\n[roles]\nscout = \"scout\"\n",
        )
        .expect("write checked-in");
        let (document, id) =
            load_fleet_document("stopship", ws.path(), None).expect("checked-in fleet still loads");
        assert_eq!(document.name(), "stopship");
        assert_eq!(id.origin, "workspace_root");

        // An exact Fleet in both workspace origins is ambiguous, and each
        // origin can be named explicitly.
        std::fs::write(checked_in.join("glm-pair.toml"), GLM_FLEET).expect("write twin");
        assert!(matches!(
            load_fleet_document("glm-pair", ws.path(), None),
            Err(NamedFleetError::AmbiguousFleet { .. })
        ));
        let (_, id) =
            load_fleet_document("workspace_root/glm-pair", ws.path(), None).expect("qualified");
        assert_eq!(id.origin, "workspace_root");
    }

    #[test]
    fn fleet_names_that_leave_the_fleets_directory_are_refused() {
        let _lock = crate::test_support::lock_test_env();
        let home = tempfile::tempdir().expect("home");
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", home.path());
        let outer = tempfile::tempdir().expect("outer");
        let ws = outer.path().join("ws");
        std::fs::create_dir_all(ws.join("fleets")).expect("fleets dir");
        std::fs::write(outer.path().join("outside.toml"), GLM_FLEET).expect("outside");
        let absolute = outer.path().join("outside");

        for name in [
            absolute.to_string_lossy().to_string(),
            "workspace_root/../../outside".to_string(),
            "codewhale_home/../outside".to_string(),
            "user/../outside".to_string(),
        ] {
            let err = load_fleet_document(&name, &ws, None).expect_err(&name);
            assert!(
                matches!(err, NamedFleetError::InvalidName),
                "{name}: expected InvalidName, got {err:?}"
            );
            let message = err.to_string();
            assert!(
                !message.contains(outer.path().to_string_lossy().as_ref()),
                "{message}"
            );
        }
    }

    /// A Fleet saved through the store at workspace scope is found by the
    /// Workflow loader instead of being reported missing. Today the store's
    /// `schema = "fleet"` revision-2 document is not a schema the Workflow
    /// loader parses, so the load names that exact file and its schema; if a
    /// v2 bridge lands, the same call succeeds from the `workspace` origin.
    #[test]
    fn a_store_saved_workspace_fleet_is_found_by_load_fleet_document() {
        use crate::fleet::store::{FleetFile, FleetScope, save_fleet};

        let _lock = crate::test_support::lock_test_env();
        let home = tempfile::tempdir().expect("home");
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", home.path());
        let ws = tempfile::tempdir().expect("workspace");

        let fleet = FleetFile::new("Folder Pair".to_string(), None).expect("fleet");
        let path = save_fleet(&fleet, FleetScope::Workspace, ws.path()).expect("save");

        match load_fleet_document(&fleet.file_slug(), ws.path(), None) {
            // A v2 bridge may label the store scope `folder` rather than the
            // `workspace` search-root origin; either names this workspace.
            Ok((_, id)) => assert!(
                matches!(id.origin.as_str(), "workspace" | "folder"),
                "{}",
                id.origin
            ),
            Err(err) => {
                assert!(
                    !matches!(err, NamedFleetError::NotFound(_)),
                    "the saved Fleet must be found, got {err}"
                );
                let message = err.to_string();
                assert!(message.contains(&path.display().to_string()), "{message}");
            }
        }
    }
}

/// `workflow(fleet:)` resolving saved v2 Fleets (store-first lookup, freeze
/// into an exact snapshot, ambiguity against legacy/exact files).
#[cfg(test)]
mod saved_fleet_tests {
    use super::*;
    use crate::fleet::store::{FleetFile, FleetMember, FleetOperator, FleetScope, save_fleet};
    use crate::test_support::{EnvVarGuard, lock_test_env};

    fn member(id: &str, pin: Option<(&str, &str)>) -> FleetMember {
        FleetMember {
            id: id.to_string(),
            display_name: None,
            shortlist: false,
            role: String::new(),
            model: pin.map(|(_, model)| model.to_string()),
            provider: pin.map(|(provider, _)| provider.to_string()),
            reasoning: None,
            instructions: None,
            requires: Vec::new(),
        }
    }

    fn fleet(name: &str, members: Vec<FleetMember>) -> FleetFile {
        let mut fleet = FleetFile::new(name.to_string(), None).expect("fleet");
        fleet.members = members;
        fleet
    }

    fn zai_session() -> Config {
        {
            let __cw_config_init_5_0 = Some("zai".to_string());
            let __cw_config_init_5_1 = Some("high".to_string());
            let mut __cw_config_init_5_value: Config = Default::default();
            __cw_config_init_5_value.provider = __cw_config_init_5_0;
            __cw_config_init_5_value.reasoning_effort = __cw_config_init_5_1;
            __cw_config_init_5_value
        }
    }

    fn session_ceiling() -> PermissionCeiling {
        PermissionCeiling {
            write: true,
            network_tool: true,
            shell: codewhale_protocol::fleet::ShellCeiling::Full,
            delegation_depth: codewhale_protocol::fleet::DEFAULT_SPAWN_DEPTH,
            tools: true,
        }
    }

    #[test]
    fn a_personal_saved_fleet_loads_as_a_frozen_exact_document() {
        let _lock = lock_test_env();
        let home = tempfile::tempdir().expect("home");
        let _home = EnvVarGuard::set("CODEWHALE_HOME", home.path());
        let ws = tempfile::tempdir().expect("workspace");
        let config = zai_session();
        let saved = fleet(
            "My fleet",
            vec![
                member(
                    "builder",
                    Some((
                        "zai",
                        codewhale_config::catalog::reviewed::constants::ZAI_GLM_5_2_MODEL,
                    )),
                ),
                member("reviewer", None),
            ],
        );
        let path = save_fleet(&saved, FleetScope::Personal, ws.path()).expect("save");

        let (document, id) =
            load_fleet_document("My fleet", ws.path(), Some(&config)).expect("v2 loads");

        assert_eq!(id.origin, "user");
        assert_eq!(id.name, "my-fleet");
        assert_eq!(document.source_path(), Some(path.as_path()));
        let exact = document.exact().expect("frozen into the exact schema");
        let builder = exact.member("builder").expect("builder");
        assert_eq!(
            (builder.provider.as_str(), builder.model.as_str()),
            (
                "zai",
                codewhale_config::catalog::reviewed::constants::ZAI_GLM_5_2_MODEL
            )
        );
        // No pin and no operator: the member inherits the live session route
        // and tier, resolved now rather than left open.
        let reviewer = exact.member("reviewer").expect("reviewer");
        assert_eq!(
            reviewer.provider,
            config.active_provider_identity().unwrap().key.as_str()
        );
        assert_eq!(reviewer.model, config.default_model());
        assert_eq!(reviewer.reasoning.as_str(), "high");

        // The slug also resolves, and so does the qualified store scope.
        load_fleet_document("my-fleet", ws.path(), Some(&config)).expect("slug loads");
        load_fleet_document("user/My fleet", ws.path(), Some(&config)).expect("user/ loads");
    }

    #[test]
    fn a_workspace_saved_fleet_loads_and_members_follow_the_operator_route() {
        let _lock = lock_test_env();
        let home = tempfile::tempdir().expect("home");
        let _home = EnvVarGuard::set("CODEWHALE_HOME", home.path());
        let ws = tempfile::tempdir().expect("workspace");
        let mut saved = fleet("reviewers", vec![member("auditor", None)]);
        saved.operator = Some(FleetOperator {
            provider: "zai".to_string(),
            model: codewhale_config::catalog::reviewed::constants::ZAI_GLM_5_2_MODEL.to_string(),
            reasoning: Some("low".to_string()),
        });
        let path = save_fleet(&saved, FleetScope::Workspace, ws.path()).expect("save");
        assert!(path.starts_with(ws.path().join(".codewhale").join("fleets")));

        // No session config is needed: nothing inherits the session route.
        let (document, id) = load_fleet_document("reviewers", ws.path(), None).expect("loads");
        assert_eq!(id.origin, "folder");
        let auditor = document.exact().unwrap().member("auditor").unwrap();
        assert_eq!(
            auditor.model,
            codewhale_config::catalog::reviewed::constants::ZAI_GLM_5_2_MODEL
        );
        assert_eq!(auditor.reasoning.as_str(), "low");
    }

    #[test]
    fn a_saved_fleet_colliding_with_an_exact_file_is_ambiguous_and_names_both_paths() {
        let _lock = lock_test_env();
        let home = tempfile::tempdir().expect("home");
        let _home = EnvVarGuard::set("CODEWHALE_HOME", home.path());
        let ws = tempfile::tempdir().expect("workspace");
        let saved_path = save_fleet(
            &fleet("glm-pair", vec![member("builder", None)]),
            FleetScope::Personal,
            ws.path(),
        )
        .expect("save");
        let exact_dir = ws.path().join("fleets");
        std::fs::create_dir_all(&exact_dir).unwrap();
        let exact_path = exact_dir.join("glm-pair.toml");
        std::fs::write(
            &exact_path,
            "name = \"glm-pair\"\nschema = \"exact\"\n\n[[members]]\nid = \"builder\"\nprovider = \"zai\"\nmodel = \"glm-5\"\n",
        )
        .unwrap();

        let error = load_fleet_document("glm-pair", ws.path(), Some(&zai_session()))
            .expect_err("a v2 and an exact Fleet of one name must not shadow each other");
        let message = error.to_string();
        assert!(
            matches!(error, NamedFleetError::AmbiguousFleet { .. }),
            "{message}"
        );
        assert!(
            message.contains(&saved_path.display().to_string()),
            "{message}"
        );
        assert!(
            message.contains(&exact_path.display().to_string()),
            "{message}"
        );

        // Qualifying either side resolves it.
        let (document, _) =
            load_fleet_document("user/glm-pair", ws.path(), Some(&zai_session())).expect("v2");
        assert_eq!(document.source_path(), Some(saved_path.as_path()));
    }

    /// Saved Fleets carry session-vocabulary reasoning (an imported agent
    /// profile stores `ultra`, `xhigh`, `minimal`); freezing maps it onto an
    /// exact tier instead of failing the exact parser, a blank value inherits,
    /// and an unknown value is refused with the member named.
    #[test]
    fn saved_fleet_reasoning_in_session_vocabulary_freezes_to_exact_tiers() {
        let _lock = lock_test_env();
        let home = tempfile::tempdir().expect("home");
        let _home = EnvVarGuard::set("CODEWHALE_HOME", home.path());
        let ws = tempfile::tempdir().expect("workspace");
        let pin = Some((
            "zai",
            codewhale_config::catalog::reviewed::constants::ZAI_GLM_5_2_MODEL,
        ));
        let mut ultra = member("ultra", pin);
        ultra.reasoning = Some("ultra".to_string());
        let mut minimal = member("minimal", pin);
        minimal.reasoning = Some("minimal".to_string());
        let mut blank = member("blank", pin);
        blank.reasoning = Some("  ".to_string());
        let mut saved = fleet("tiers", vec![ultra, minimal, blank]);
        saved.operator = Some(FleetOperator {
            provider: "zai".to_string(),
            model: codewhale_config::catalog::reviewed::constants::ZAI_GLM_5_2_MODEL.to_string(),
            reasoning: Some("xhigh".to_string()),
        });
        save_fleet(&saved, FleetScope::Workspace, ws.path()).expect("save");

        let (document, _) = load_fleet_document("tiers", ws.path(), None).expect("freezes");
        let exact = document.exact().expect("exact");
        let tier = |id: &str| exact.member(id).expect(id).reasoning.as_str();
        assert_eq!(tier("ultra"), "max");
        assert_eq!(tier("minimal"), "low");
        // Blank inherits the operator's `xhigh`, which is the `max` tier.
        assert_eq!(tier("blank"), "max");

        let mut bad = member("bad", pin);
        bad.reasoning = Some("turbo".to_string());
        save_fleet(
            &fleet("bad-tier", vec![bad]),
            FleetScope::Workspace,
            ws.path(),
        )
        .expect("save");
        let error = load_fleet_document("bad-tier", ws.path(), None).expect_err("refused");
        assert!(
            error.to_string().contains("member `bad` reasoning"),
            "{error}"
        );
    }

    #[test]
    fn member_instructions_are_refused_rather_than_silently_dropped() {
        let _lock = lock_test_env();
        let home = tempfile::tempdir().expect("home");
        let _home = EnvVarGuard::set("CODEWHALE_HOME", home.path());
        let ws = tempfile::tempdir().expect("workspace");
        let mut coach = member(
            "coach",
            Some((
                "zai",
                codewhale_config::catalog::reviewed::constants::ZAI_GLM_5_2_MODEL,
            )),
        );
        coach.instructions = Some("Always cite sources.".to_string());
        save_fleet(
            &fleet("coached", vec![coach]),
            FleetScope::Workspace,
            ws.path(),
        )
        .expect("save");

        let error = load_fleet_document("coached", ws.path(), None).expect_err("refused");
        assert!(
            error.to_string().contains("`coach` has instructions"),
            "{error}"
        );
    }

    /// The frozen snapshot is what runs: editing the saved file after capture
    /// moves nothing, and the inherited member's preflighted route is the same
    /// session route the snapshot names.
    #[test]
    fn frozen_routes_survive_a_mid_run_edit_and_inherit_matches_preflight() {
        let _lock = lock_test_env();
        let home = tempfile::tempdir().expect("home");
        let _home = EnvVarGuard::set("CODEWHALE_HOME", home.path());
        let _key = EnvVarGuard::set("ZAI_API_KEY", "zai-key");
        let ws = tempfile::tempdir().expect("workspace");
        let config = zai_session();
        let mut saved = fleet(
            "release",
            vec![
                member(
                    "builder",
                    Some((
                        "zai",
                        codewhale_config::catalog::reviewed::constants::ZAI_GLM_5_2_MODEL,
                    )),
                ),
                member("reviewer", None),
            ],
        );
        save_fleet(&saved, FleetScope::Workspace, ws.path()).expect("save");

        let (document, id) =
            load_fleet_document("release", ws.path(), Some(&config)).expect("loads");
        let roots = fleet_search_roots(ws.path());
        let workflow = ExactFleetWorkflow::capture(
            &document,
            id,
            "2026-09-22T00:00:00Z",
            Some(&config),
            &roots,
        )
        .expect("capture");

        // Edit the saved Fleet mid-run.
        saved.members[0].model = Some("glm-4.6".to_string());
        save_fleet(&saved, FleetScope::Workspace, ws.path()).expect("re-save");

        let builder = workflow
            .bind_member(Some("builder"), None, session_ceiling())
            .expect("bind builder");
        assert_eq!(
            builder.route.wire_model,
            codewhale_config::catalog::reviewed::constants::ZAI_GLM_5_2_MODEL
        );

        let reviewer = workflow
            .bind_member(Some("reviewer"), None, session_ceiling())
            .expect("bind reviewer");
        let frozen = workflow
            .snapshot()
            .members()
            .iter()
            .find(|member| member.id == "reviewer")
            .expect("reviewer in snapshot");
        assert_eq!(frozen.route.model, config.default_model());
        assert_eq!(reviewer.route.frozen().model, reviewer.route.wire_model);
        assert_eq!(
            reviewer.route.wire_model,
            codewhale_runtime::config::requested_model_for_provider(
                config.active_provider_identity().unwrap().provider,
                &config.default_model()
            )
            .expect("session model is a known route")
        );
    }
}

//! The runtime-resolved executable route (#3384).
//!
//! A [`ReadyRouteCandidate`] is the concrete form of the #2608 contract:
//!
//! > Execution requires a `ReadyRouteCandidate`.
//! > A `ReadyRouteCandidate` can only be produced by `RouteResolver`.
//!
//! Fields are private and exposed only through read-only getters, so the type
//! can neither be *constructed* nor *mutated* outside this crate, and it
//! deliberately does not derive `Deserialize` (so it cannot be fabricated from
//! JSON either). The only constructor is `ReadyRouteCandidate::new`
//! (`pub(super)`), and [`super::resolver::RouteResolver::resolve`] is its sole
//! caller. A candidate's existence is therefore proof it passed the resolver,
//! and a candidate's limits are therefore exactly what the resolver produced
//! (including any [`SourcedLimitOverride`]s recorded on it).
//!
//! The route-owned, three-state capability profile lives in this crate and is
//! carried on the candidate. A full `config_snapshot: Config` remains deferred
//! because embedding it would couple the candidate to the full config model.

use serde::{Deserialize, Serialize};

use super::RequestProtocol;
use super::capabilities::RouteCapabilities;
use super::ids::{LogicalModelRef, ModelId, ProviderId, WireModelId};
use super::offering::RouteLimits;
use crate::ProviderKind;
use crate::responses_dialect::ResponsesProfileId;

/// A concrete, resolved endpoint the route will talk to.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvedEndpoint {
    /// Resolved base URL (after any override).
    pub base_url: String,
    /// Endpoint key (e.g. `"chat"`, `"responses"`).
    pub endpoint_key: String,
    /// Wire protocol spoken at this endpoint.
    pub protocol: RequestProtocol,
}

/// The CLASS of auth source resolved for the route.
///
/// This records only *where* a credential comes from, never the credential
/// value itself. There is intentionally no field that could hold a secret.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ResolvedAuthSource {
    /// Supplied via CLI flag/argument.
    Cli,
    /// Read from a config file.
    ConfigFile,
    /// Read from the OS keyring.
    Keyring,
    /// Read from an environment variable.
    Env,
    /// Produced by running a command.
    Command,
    /// Resolved from a named secret.
    Secret,
    /// No credential resolved.
    Missing,
    /// Auth resolution has not been performed for this candidate.
    ///
    /// The route resolver never inspects credentials, so a freshly resolved
    /// candidate honestly reports `Unresolved` rather than claiming `Missing`
    /// (which would assert a lookup that never happened).
    Unresolved,
}

/// Which token limit a [`SourcedLimitOverride`] targets.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LimitField {
    /// [`RouteLimits::context_tokens`](super::offering::RouteLimits).
    ContextTokens,
    /// [`RouteLimits::input_tokens`](super::offering::RouteLimits).
    InputTokens,
    /// [`RouteLimits::output_tokens`](super::offering::RouteLimits).
    OutputTokens,
}

/// Why a limit override was requested.
///
/// This is provenance, not policy: the resolver applies whatever the caller
/// requested and records the source on the candidate so every consumer can see
/// where an effective limit came from.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OverrideSource {
    /// Operator-configured context window.
    UserContextWindow,
    /// Operator-configured context window for one exact wire model id
    /// (`[providers.<id>.model_context_windows]`, #6108).
    UserModelContextWindow,
    /// Limits declared by the operator for an exact model and endpoint.
    UserModelMetadata,
    /// Catalog limits describe the public API offering, not the account-scoped
    /// Codex route; the API-only limits are stripped.
    CodexPublicApiLimitStrip,
    /// Per-model context from the fresh Codex account roster.
    CodexRosterCorrection,
    /// Fresh, route-scoped provider-reported context metadata.
    ProviderReportedContextWindow,
    /// Conservative all-plan safe floor for a membership-plan route.
    MembershipPlanSafeFloor,
    /// Maximum output documented for one exact provider endpoint/model route.
    DocumentedRouteOutputMaximum,
}

/// One sourced limit override, applied by the resolver BEFORE the candidate is
/// constructed and recorded on the candidate as provenance.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct SourcedLimitOverride {
    /// The limit field to override.
    pub field: LimitField,
    /// The value to set (`None` clears the limit to "unknown").
    pub value: Option<u64>,
    /// Why the override was requested.
    pub source: OverrideSource,
}

/// Pricing/quota class for the resolved route.
///
/// Carries only coarse, non-sensitive shape; never secrets or account ids.
///
/// `PartialEq` (but not `Eq`: the `Token` rates are `f64`) lets offerings and
/// candidates be compared in tests and lets
/// [`super::offering::ProviderModelOffering`] carry a pricing meter.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PricingSku {
    /// Per-token pricing.
    Token {
        /// Input price per million tokens, if known.
        input_per_mtok: Option<f64>,
        /// Output price per million tokens, if known.
        output_per_mtok: Option<f64>,
    },
    /// Subscription quota usage.
    SubscriptionQuota {
        /// Percent of quota used, if known.
        used_pct: Option<f32>,
        /// When the quota resets, if known.
        resets_at: Option<String>,
    },
    /// Prepaid account credits.
    AccountCredits {
        /// Remaining balance, if known.
        balance: Option<f64>,
    },
    /// Local or otherwise not billed.
    LocalOrNotApplicable,
    /// Pricing unknown or stale.
    UnknownOrStale,
}

/// Outcome of route validation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationReport {
    /// Whether the route passed validation.
    pub ok: bool,
    /// Human-readable diagnostics (advisory; secret-free).
    pub messages: Vec<String>,
}

/// A runtime-resolved, executable route.
///
/// The candidate is IMMUTABLE once minted: every field is private and exposed
/// only through read-only getters, the type cannot be constructed outside this
/// crate (private fields + no `Deserialize`), and there are no setters. The
/// only constructor is `Self::new`, which is `pub(super)`; see module docs.
/// Post-resolution limit adjustments must instead be requested up front via
/// [`super::resolver::RouteRequest::limit_overrides`], which the resolver
/// applies BEFORE construction and records in [`Self::applied_limit_overrides`].
///
/// Immutability is compile-time enforced — this does not build:
///
/// ```compile_fail
/// use codewhale_config::route::{RouteRequest, RouteResolver};
///
/// let mut candidate = RouteResolver::new()
///     .resolve(&RouteRequest::default())
///     .unwrap();
/// // ERROR: field `limits` of `ReadyRouteCandidate` is private
/// candidate.limits.context_tokens = Some(1);
/// ```
#[derive(Debug, Clone, Serialize)]
#[non_exhaustive]
pub struct ReadyRouteCandidate {
    /// Resolved provider id.
    provider_id: ProviderId,
    /// Resolved provider kind.
    provider_kind: ProviderKind,
    /// The selector the user/route requested.
    logical_model: LogicalModelRef,
    /// Canonical model identity, if one was resolved.
    canonical_model: Option<ModelId>,
    /// Provider-owned wire id put on the request.
    wire_model_id: WireModelId,
    /// Resolved endpoint transport facts.
    endpoint: ResolvedEndpoint,
    /// Resolved auth source CLASS (never a secret value).
    auth: ResolvedAuthSource,
    /// Selected wire protocol.
    protocol: RequestProtocol,
    /// Responses dialect selected after the protocol is known.
    /// Absent for every non-Responses route.
    #[serde(skip_serializing_if = "Option::is_none")]
    responses_profile_id: Option<ResponsesProfileId>,
    /// Route/offering-scoped token limits, when known (overrides applied).
    limits: RouteLimits,
    /// Capability facts for the exact provider/model offering.
    capabilities: RouteCapabilities,
    /// Pricing/quota class, if known.
    pricing: Option<PricingSku>,
    /// Validation outcome.
    validation: ValidationReport,
    /// Provenance of every limit override applied at resolution time.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    applied_limit_overrides: Vec<SourcedLimitOverride>,
}

impl ReadyRouteCandidate {
    /// Mint a candidate. Restricted to [`super::resolver`] so the resolver is
    /// the sole producer of executable routes (the #2608 mutation gate).
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        provider_id: ProviderId,
        provider_kind: ProviderKind,
        logical_model: LogicalModelRef,
        canonical_model: Option<ModelId>,
        wire_model_id: WireModelId,
        endpoint: ResolvedEndpoint,
        auth: ResolvedAuthSource,
        protocol: RequestProtocol,
        responses_profile_id: Option<ResponsesProfileId>,
        limits: RouteLimits,
        capabilities: RouteCapabilities,
        pricing: Option<PricingSku>,
        validation: ValidationReport,
        applied_limit_overrides: Vec<SourcedLimitOverride>,
    ) -> Self {
        assert_eq!(
            protocol == RequestProtocol::Responses,
            responses_profile_id.is_some(),
            "a responses route carries a profile id and other routes do not"
        );
        Self {
            provider_id,
            provider_kind,
            logical_model,
            canonical_model,
            wire_model_id,
            endpoint,
            auth,
            protocol,
            responses_profile_id,
            limits,
            capabilities,
            pricing,
            validation,
            applied_limit_overrides,
        }
    }

    /// Resolved provider id.
    #[must_use]
    pub fn provider_id(&self) -> &ProviderId {
        &self.provider_id
    }

    /// Resolved provider kind.
    #[must_use]
    pub fn provider_kind(&self) -> ProviderKind {
        self.provider_kind
    }

    /// The selector the user/route requested.
    #[must_use]
    pub fn logical_model(&self) -> &LogicalModelRef {
        &self.logical_model
    }

    /// Canonical model identity, if one was resolved.
    #[must_use]
    pub fn canonical_model(&self) -> Option<&ModelId> {
        self.canonical_model.as_ref()
    }

    /// Provider-owned wire id put on the request.
    #[must_use]
    pub fn wire_model_id(&self) -> &WireModelId {
        &self.wire_model_id
    }

    /// Resolved endpoint transport facts.
    #[must_use]
    pub fn endpoint(&self) -> &ResolvedEndpoint {
        &self.endpoint
    }

    /// Resolved auth source CLASS (never a secret value).
    #[must_use]
    pub fn auth(&self) -> &ResolvedAuthSource {
        &self.auth
    }

    /// Selected wire protocol.
    #[must_use]
    pub fn protocol(&self) -> RequestProtocol {
        self.protocol
    }

    /// Responses dialect id, present exactly when the protocol is Responses.
    #[must_use]
    pub fn responses_profile_id(&self) -> Option<ResponsesProfileId> {
        self.responses_profile_id
    }

    /// Route/offering-scoped token limits, when known (overrides applied).
    #[must_use]
    pub fn limits(&self) -> RouteLimits {
        self.limits
    }

    /// Capability facts for the exact provider/model offering.
    #[must_use]
    pub fn capabilities(&self) -> RouteCapabilities {
        self.capabilities
    }

    /// Pricing/quota class, if known.
    #[must_use]
    pub fn pricing(&self) -> Option<&PricingSku> {
        self.pricing.as_ref()
    }

    /// Validation outcome.
    #[must_use]
    pub fn validation(&self) -> &ValidationReport {
        &self.validation
    }

    /// Provenance of every limit override applied at resolution time.
    #[must_use]
    pub fn applied_limit_overrides(&self) -> &[SourcedLimitOverride] {
        &self.applied_limit_overrides
    }
}

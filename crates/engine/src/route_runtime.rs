use std::collections::BTreeMap;

use chrono::{DateTime, Duration, Utc};
use codewhale_config::route::{
    LimitField, LogicalModelRef, OverrideSource, ReadyRouteCandidate, RouteError, RouteLimits,
    RouteRequest, RouteResolver, SourcedLimitOverride, WireModelId,
};
use serde::{Deserialize, Serialize};

use crate::client::CodewhaleClient;
use crate::codex_model_cache::model_roster_for;
use codewhale_config::ProviderKind;
use codewhale_models::DIRECT_KIMI_K3_MAX_OUTPUT_TOKENS;
use codewhale_models::KIMI_CODE_K3_CONTEXT_WINDOW_TOKENS;
use codewhale_runtime::codex_model_roster::{CodexModelCacheFreshness, CodexModelRoster};
use codewhale_runtime::config::{
    Config, ProviderIdentity, is_exact_direct_moonshot_k3_route, is_exact_kimi_code_bare_k3_route,
    validate_kimi_code_api_model_id,
};

/// Why a route is using its effective context-window value.  Keep this
/// receipt separate from the numeric route limits so every consumer can state
/// whether the number is operator-configured, freshly provider-reported, a
/// Kimi Code safety floor, catalog data, or a conservative fallback.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextWindowSource {
    Configured,
    /// `[providers.<id>.model_context_windows]` hit for this exact wire model
    /// id — outranks the provider-level `Configured` rung (#6108).
    ConfiguredModel,
    UserDeclared,
    ProviderReported,
    StaticKimiCodeSafeFloor,
    Catalog,
    /// Parsed from a vendor-agnostic `_Nk` suffix in the model name
    /// (#5441). Optimistic, unlike the conservative [`Self::Fallback`]: a
    /// serving engine may ignore its own naming convention, so the number
    /// drives real budgets but is never evidence about the route.
    NameSuffixHint,
    Fallback,
}

impl ContextWindowSource {
    /// Every rung, in precedence order. The name-suffix hint sits between
    /// catalog data and the conservative fallback: any concrete fact about
    /// the route beats a naming convention.
    pub const ALL: [Self; 8] = [
        Self::ConfiguredModel,
        Self::Configured,
        Self::UserDeclared,
        Self::ProviderReported,
        Self::StaticKimiCodeSafeFloor,
        Self::Catalog,
        Self::NameSuffixHint,
        Self::Fallback,
    ];

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Configured => "configured",
            Self::ConfiguredModel => "configured (per-model)",
            Self::UserDeclared => "user declared",
            Self::ProviderReported => "provider-reported",
            Self::StaticKimiCodeSafeFloor => "static Kimi Code safe floor",
            Self::Catalog => "catalog",
            Self::NameSuffixHint => "model-name hint",
            Self::Fallback => "fallback",
        }
    }

    /// Recover the rung a serialized report wrote, so a surface holding only
    /// the label still reads verification off the enum instead of matching
    /// strings.  An unrecognized label is nobody's rung.
    #[must_use]
    pub fn from_label(label: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|rung| rung.label() == label)
    }

    /// Whether the window rests on evidence about this exact route.  The
    /// name-suffix hint and the fallback rung are guesses — one parsed from a
    /// naming convention, one made because nothing described the model — so
    /// no surface may present either as a capability we checked (#5239,
    /// #5441).
    #[must_use]
    pub const fn is_verified(self) -> bool {
        !matches!(
            self,
            Self::NameSuffixHint | Self::Fallback | Self::UserDeclared
        )
    }

    /// Suffix every rendered window carries: verified rungs stay bare,
    /// guesses say so next to the number that drives the budget.
    #[must_use]
    pub const fn honesty_suffix(self) -> &'static str {
        if self.is_verified() {
            ""
        } else {
            " (unverified)"
        }
    }

    /// [`Self::label`] plus [`Self::honesty_suffix`], ready for inline
    /// rendering (status line, `/status`, `/config` rows).
    #[must_use]
    pub fn display_label(self) -> String {
        format!("{}{}", self.label(), self.honesty_suffix())
    }
}

/// Context window carried alongside an exact runtime route.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextWindowResolution {
    pub tokens: u32,
    pub source: ContextWindowSource,
}

impl ContextWindowResolution {
    pub fn instruction_budget_tokens(self) -> Option<usize> {
        (self.tokens > 0
            && !matches!(
                self.source,
                ContextWindowSource::Fallback | ContextWindowSource::NameSuffixHint
            ))
        .then_some(self.tokens as usize)
    }
}

/// Resolve the effective context window for a host holding no fully resolved
/// route candidate: an `auto` selection, a model switch that keeps the current
/// endpoint, or a route resolution that failed.
///
/// Only the rungs derivable without an endpoint-scoped candidate are reachable
/// here — operator config, then offering/catalog limits, then the conservative
/// capability fallback.  The provider-reported and Kimi Code safe-floor rungs
/// need a resolved candidate and stay in [`plan_limit_overrides`].
///
/// The catalog predicate must stay identical to the one in
/// [`crate::route_budget::route_context_window_tokens`]: the pressure meter and
/// compaction trigger read their number from there, so any divergence would
/// print one rung's number under another rung's label.
#[must_use]
pub fn resolve_context_window(
    provider: ProviderKind,
    model: &str,
    route_limits: Option<RouteLimits>,
    context_window_override: Option<u32>,
    model_context_windows: Option<&BTreeMap<String, u32>>,
) -> ContextWindowResolution {
    if let Some(tokens) = model_context_windows
        .and_then(|table| table.get(model).copied())
        .filter(|tokens| *tokens > 0)
    {
        return ContextWindowResolution {
            tokens,
            source: ContextWindowSource::ConfiguredModel,
        };
    }
    if let Some(tokens) = context_window_override.filter(|tokens| *tokens > 0) {
        return ContextWindowResolution {
            tokens,
            source: ContextWindowSource::Configured,
        };
    }
    if let Some(tokens) = route_limits
        .and_then(|limits| limits.context_tokens)
        .and_then(|tokens| u32::try_from(tokens).ok())
        .filter(|tokens| *tokens > 0)
    {
        return ContextWindowResolution {
            tokens,
            source: ContextWindowSource::Catalog,
        };
    }
    let tokens = crate::route_budget::route_context_window_tokens(provider, model, None);
    ContextWindowResolution {
        tokens,
        source: classify_capability_fallback_window(model, tokens),
    }
}

/// Classify a window the provider/model capability fallback produced, so the
/// receipt names the rung the number actually came from (#5239, #5441).
///
/// A value parsed from an `_Nk` model-name suffix is its own optimistic rung:
/// the serving engine may not honor its own naming convention. Everything
/// else the fallback produced — vendor-family heuristics, provider floors,
/// the conservative default — is the plain fallback rung. Both are
/// unverified; the ladder keeps them apart because they fail differently
/// (a hint that overstates the window delays compaction past the provider's
/// real limit).
fn classify_capability_fallback_window(model: &str, tokens: u32) -> ContextWindowSource {
    if codewhale_models::name_suffix_context_window_hint(model) == Some(tokens) {
        ContextWindowSource::NameSuffixHint
    } else {
        ContextWindowSource::Fallback
    }
}

/// Authenticated Kimi Code `/models` metadata that a caller has already
/// validated.  This is intentionally route-scoped: generic Moonshot metadata
/// can never promote a bare `k3` route.  The current runtime has no implicit
/// network probe; an authenticated model-listing consumer may pass this value
/// to [`resolve_route_candidate_with_context_metadata`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderReportedKimiCodeContext {
    pub context_tokens: u32,
    pub observed_at: DateTime<Utc>,
}

const KIMI_CODE_REPORTED_CONTEXT_MAX_AGE_HOURS: i64 = 24;

#[derive(Debug)]
pub struct RouteCandidateResolution {
    pub candidate: ReadyRouteCandidate,
    pub context_window: ContextWindowResolution,
}

#[derive(Clone)]
pub struct ResolvedRuntimeRoute {
    pub identity: ProviderIdentity,
    pub candidate: ReadyRouteCandidate,
    pub config: Box<Config>,
    pub model: String,
    pub context_window: ContextWindowResolution,
    preflighted_client: Option<Box<CodewhaleClient>>,
}

impl std::fmt::Debug for ResolvedRuntimeRoute {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResolvedRuntimeRoute")
            .field("provider_identity", &self.identity.key)
            .field("provider", &self.identity.provider)
            .field("model", &self.model)
            .finish_non_exhaustive()
    }
}

/// One exact provider route, fully resolved and client-preflighted before a
/// host mutates session/runtime state. The config and client may contain
/// credentials, so diagnostics intentionally expose only non-secret receipt
/// fields.
#[derive(Clone)]
pub struct ValidatedRuntimeRoute {
    pub identity: ProviderIdentity,
    pub candidate: ReadyRouteCandidate,
    pub config: Box<Config>,
    pub model: String,
    pub context_window: ContextWindowResolution,
    pub client: CodewhaleClient,
}

impl std::fmt::Debug for ValidatedRuntimeRoute {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ValidatedRuntimeRoute")
            .field("provider_identity", &self.identity.key)
            .field("provider", &self.identity.provider)
            .field("model", &self.model)
            .finish_non_exhaustive()
    }
}

/// Who reads a route preflight failure. The interactive app can run slash
/// commands; a headless caller only has the CLI. Only `codewhale exec` asks
/// for [`Self::Headless`] today (through [`ResolvedRuntimeRoute::validate_for`]);
/// `preflight` and other callers keep the interactive wording.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteErrorSurface {
    Interactive,
    Headless,
}

impl ResolvedRuntimeRoute {
    pub fn preflight(mut self) -> Result<Self, String> {
        self.config.verify_provider_identity(&self.identity)?;
        if self.preflighted_client.is_none() {
            self.preflighted_client = Some(Box::new(
                CodewhaleClient::from_candidate(&self.config, &self.candidate)
                    .map_err(|err| {
                        format_provider_route_preflight_error(
                            self.identity.key.as_str(),
                            &self.model,
                            &err,
                            RouteErrorSurface::Interactive,
                        )
                    })?
                    .with_context_window(self.context_window),
            ));
        }
        Ok(self)
    }

    pub fn validate(self) -> Result<ValidatedRuntimeRoute, String> {
        self.validate_for(RouteErrorSurface::Interactive)
    }

    /// [`Self::validate`] with next steps worded for `surface`.
    pub fn validate_for(
        mut self,
        surface: RouteErrorSurface,
    ) -> Result<ValidatedRuntimeRoute, String> {
        self.config.verify_provider_identity(&self.identity)?;
        let client = match self.preflighted_client.take() {
            Some(client) => *client,
            None => {
                CodewhaleClient::from_candidate(&self.config, &self.candidate).map_err(|err| {
                    format_provider_route_preflight_error(
                        self.identity.key.as_str(),
                        &self.model,
                        &err,
                        surface,
                    )
                })?
            }
        }
        .with_context_window(self.context_window);
        if client.admitted_provider_identity() != &self.identity {
            return Err("preflighted client does not match captured provider identity".into());
        }
        Ok(ValidatedRuntimeRoute {
            identity: self.identity,
            candidate: self.candidate,
            config: self.config,
            model: self.model,
            context_window: self.context_window,
            client,
        })
    }

    pub fn take_preflighted_client(&mut self) -> Option<CodewhaleClient> {
        self.preflighted_client.take().map(|client| *client)
    }
}

fn format_provider_route_preflight_error(
    identity_key: &str,
    model: &str,
    err: &anyhow::Error,
    surface: RouteErrorSurface,
) -> String {
    let reason = err.to_string();
    let reason = reason.trim();
    // Multi-line guidance ends in a command to copy; a period appended to it
    // would break the paste, so it gets a line break instead.
    let multi_line = reason.contains('\n');
    let mut message = if multi_line {
        format!("{reason}\nFailed to configure provider route {identity_key} / {model}.")
    } else {
        format!(
            "{}. Failed to configure provider route {identity_key} / {model}.",
            reason.trim_end_matches('.')
        )
    };
    if let Some(next_step) =
        classify_provider_route_preflight_next_step(identity_key, reason, surface)
    {
        message.push_str(if multi_line { "\n" } else { " " });
        message.push_str("Next step: ");
        message.push_str(&next_step);
    }
    message
}

fn classify_provider_route_preflight_next_step(
    identity_key: &str,
    reason: &str,
    surface: RouteErrorSurface,
) -> Option<String> {
    let headless = surface == RouteErrorSurface::Headless;
    let lower = reason.to_ascii_lowercase();
    if lower.contains("chatgpt credentials are only available on the official public api route") {
        return Some(if headless {
            format!(
                "Remove the custom base_url from [providers.{identity_key}]; ChatGPT plan access only works on the official public API route."
            )
        } else {
            format!(
                "Run /provider setup {identity_key} and remove its custom base URL; ChatGPT plan access only works on the official public API route."
            )
        });
    }
    if lower.contains("sign in with chatgpt")
        || lower.contains("chatgpt credentials")
        || lower.contains("chatgpt grant")
        || lower.contains("chatgpt registration")
        || lower.contains("openai codex oauth credentials are unavailable")
        || lower.contains("codex access token")
    {
        return Some(if headless {
            "Run `codewhale auth chatgpt` to Sign in with ChatGPT.".to_string()
        } else {
            format!(
                "Run `codewhale auth chatgpt` or /provider setup {identity_key} to Sign in with ChatGPT."
            )
        });
    }
    if lower.contains("api key not found")
        || lower.contains("access token")
        || (lower.contains("credential")
            && (lower.contains("not found")
                || lower.contains("missing")
                || lower.contains("unsupported")))
    {
        if !headless {
            return Some(format!(
                "Run /auth or /provider setup {identity_key} to configure credentials."
            ));
        }
        // The reason usually names its own command (`codewhale auth set`,
        // `codewhale auth chatgpt`, `codewhale auth xai-device`, ...);
        // repeating one would make two instructions out of one.
        if lower.contains("codewhale auth") {
            return None;
        }
        // `auth set` stores an API key. An access-token or other credential
        // failure may belong to an OAuth route, where it is the wrong fix.
        if !lower.contains("api key") {
            return Some(
                "Run `codewhale doctor` to see which credential this route needs.".to_string(),
            );
        }
        return Some(
            match ProviderKind::parse(identity_key).filter(|p| *p != ProviderKind::Custom) {
                Some(provider) => {
                    format!("Run `codewhale auth set --provider {}`.", provider.as_str())
                }
                // A custom identity's key is its `[providers.<name>]` table.
                None => format!(
                    "Add api_key or api_key_env to [providers.{identity_key}] in config.toml."
                ),
            },
        );
    }
    if lower.contains("tls certificate")
        || lower.contains("ssl_cert_file")
        || lower.contains("certificate verification")
        || lower.contains("insecure_skip_tls_verify")
        || lower.contains("base url")
        || lower.contains("invalid url")
    {
        return Some(if headless {
            format!("Fix base_url/TLS settings in [providers.{identity_key}] of config.toml.")
        } else {
            format!("Run /provider setup {identity_key} to fix base URL/TLS settings.")
        });
    }
    if lower.contains("provider")
        && lower.contains("model")
        && (lower.contains("pin")
            || lower.contains("mismatch")
            || lower.contains("unknown")
            || lower.contains("not found"))
    {
        return Some(if headless {
            "Pass a model this provider serves with --model (`codewhale models` lists them)."
                .to_string()
        } else {
            "Run /models (or open the model picker) and choose a model valid for this provider."
                .to_string()
        });
    }
    if lower.contains("fleet") || lower.contains("profile") || lower.contains("partial route") {
        return Some(
            "Review Fleet profile provider/model overrides; keep route fields atomic (#5042)."
                .to_string(),
        );
    }
    Some(if headless {
        "Run `codewhale doctor` to review this route configuration.".to_string()
    } else {
        format!("Run /provider setup {identity_key} to review this route configuration.")
    })
}

impl ValidatedRuntimeRoute {
    /// Preserve the preflighted client with the exact resolved route receipt
    /// so the engine does not repeat environment-sensitive client discovery.
    pub fn into_resolved(self) -> ResolvedRuntimeRoute {
        ResolvedRuntimeRoute {
            identity: self.identity,
            candidate: self.candidate,
            config: self.config,
            model: self.model,
            context_window: self.context_window,
            preflighted_client: Some(Box::new(self.client)),
        }
    }
}

pub fn resolve_route_candidate(
    provider: ProviderKind,
    model_selector: Option<&str>,
    saved_provider_model: Option<&str>,
    base_url_override: Option<String>,
    context_window_override: Option<u32>,
    model_context_windows: Option<&BTreeMap<String, u32>>,
) -> Result<ReadyRouteCandidate, String> {
    resolve_route_candidate_with_context_metadata(
        provider,
        model_selector,
        saved_provider_model,
        base_url_override,
        context_window_override,
        model_context_windows,
        None,
    )
    .map(|resolution| resolution.candidate)
}

/// Reject only a provider-less model mismatch that existing route knowledge
/// proves foreign. Partial catalogs are not allowlists: unknown ids, local
/// runtimes, gateways, and custom endpoints remain provider-authoritative.
pub fn validate_unpinned_model_provider(
    provider: ProviderKind,
    model: &str,
    base_url: &str,
) -> Result<(), String> {
    let kind = provider;
    let Some(owner) = codewhale_config::known_foreign_model_owner(kind, model, base_url) else {
        return Ok(());
    };
    Err(format!(
        "Model `{}` was supplied without an explicit provider pin, but the resolved route is `{}` and the owning provider is `{}`. Pin the provider together with the model, or inherit the session route.",
        model.trim(),
        provider.as_str(),
        owner.as_str()
    ))
}

/// Resolve a provider-less fixed model to the provider's exact wire id before
/// child admission. This shares the runtime resolver used by Fleet receipts,
/// including aggregator alias translation, without making a live request.
#[cfg(any(test, feature = "test-support"))]
pub fn resolve_unpinned_model_candidate(
    provider: ProviderKind,
    model: &str,
    base_url: &str,
) -> Result<ReadyRouteCandidate, String> {
    validate_unpinned_model_provider(provider, model, base_url)?;
    resolve_route_candidate(
        provider,
        Some(model),
        None,
        Some(base_url.to_string()),
        None,
        None,
    )
}

/// Resolve a candidate together with a non-secret context-window provenance
/// receipt.  `provider_reported_context` is accepted only for the exact Kimi
/// Code bare-K3 endpoint, only at the documented 1M entitlement, and only
/// while fresh; this prevents generic Moonshot or stale metadata from being
/// inherited by a membership-plan route.
/// Resolve a manual selection from the App's loaded, non-secret metadata
/// snapshot. This shares the same scoped resolver and limit precedence as
/// config-backed runtime selection, without loading credentials while typing.
pub fn resolve_declared_model_candidate(
    provider: ProviderKind,
    identity: &str,
    model: &str,
    base_url: &str,
    context_window: Option<u32>,
    model_context_windows: Option<&BTreeMap<String, u32>>,
    models: &[codewhale_config::catalog::configured::ConfiguredModel],
) -> Result<RouteCandidateResolution, String> {
    resolve_declared_model_candidate_with_wire_override(
        provider,
        identity,
        model,
        base_url,
        context_window,
        model_context_windows,
        models,
        None,
    )
}

fn live_route_resolver(
    provider: ProviderKind,
    identity: Option<&str>,
    base_url: &str,
) -> (RouteResolver, bool, bool) {
    let identity = identity
        .map(str::trim)
        .filter(|identity| !identity.is_empty());
    let unkeyed_custom = provider == ProviderKind::Custom && identity.is_none();
    if matches!(provider, ProviderKind::OpenaiCodex) || unkeyed_custom {
        return (RouteResolver::new(), false, false);
    }
    let identity = identity.unwrap_or_else(|| provider.as_str());
    let catalog = crate::provider_lake::runtime_catalog_resolver_for_identity(
        provider,
        Some(identity),
        base_url,
        crate::provider_catalog_live::status_for_route(provider, identity, base_url),
    );
    (
        catalog.resolver,
        catalog.endpoint_catalog_authoritative,
        true,
    )
}

/// The picker-facing variant: a browsing row of a custom table passes the
/// row's own `wire` dialect so the receipt's protocol label matches the wire
/// a turn would mint.
pub fn resolve_declared_model_candidate_with_wire_override(
    provider: ProviderKind,
    identity: &str,
    model: &str,
    base_url: &str,
    context_window: Option<u32>,
    model_context_windows: Option<&BTreeMap<String, u32>>,
    models: &[codewhale_config::catalog::configured::ConfiguredModel],
    wire_override: Option<codewhale_config::provider::WireFormat>,
) -> Result<RouteCandidateResolution, String> {
    let (resolver, endpoint_catalog_authoritative, resolver_reads_models_dev) =
        live_route_resolver(provider, Some(identity), base_url);
    let resolver = resolver.with_configured_models(models, identity, provider, base_url);
    resolve_route_candidate_with_catalog_resolver(
        provider,
        Some(model),
        None,
        Some(base_url.into()),
        context_window,
        model_context_windows,
        None,
        None,
        &resolver,
        endpoint_catalog_authoritative,
        resolver_reads_models_dev,
        wire_override,
    )
}

pub fn resolve_route_candidate_with_context_metadata(
    provider: ProviderKind,
    model_selector: Option<&str>,
    saved_provider_model: Option<&str>,
    base_url_override: Option<String>,
    context_window_override: Option<u32>,
    model_context_windows: Option<&BTreeMap<String, u32>>,
    provider_reported_context: Option<ProviderReportedKimiCodeContext>,
) -> Result<RouteCandidateResolution, String> {
    resolve_route_candidate_with_context_metadata_and_wire_override(
        provider,
        model_selector,
        saved_provider_model,
        base_url_override,
        context_window_override,
        model_context_windows,
        provider_reported_context,
        None,
    )
}

/// Snapshot variant carrying a custom table's wire dialect (see
/// [`resolve_declared_model_candidate_with_wire_override`]).
pub fn resolve_route_candidate_with_context_metadata_and_wire_override(
    provider: ProviderKind,
    model_selector: Option<&str>,
    saved_provider_model: Option<&str>,
    base_url_override: Option<String>,
    context_window_override: Option<u32>,
    model_context_windows: Option<&BTreeMap<String, u32>>,
    provider_reported_context: Option<ProviderReportedKimiCodeContext>,
    wire_override: Option<codewhale_config::provider::WireFormat>,
) -> Result<RouteCandidateResolution, String> {
    let resolver_base_url = base_url_override
        .clone()
        .unwrap_or_else(|| provider.provider().default_base_url().to_string());
    let (resolver, endpoint_catalog_authoritative, resolver_reads_models_dev) =
        live_route_resolver(provider, None, &resolver_base_url);
    resolve_route_candidate_with_catalog_resolver(
        provider,
        model_selector,
        saved_provider_model,
        base_url_override,
        context_window_override,
        model_context_windows,
        provider_reported_context,
        None,
        &resolver,
        endpoint_catalog_authoritative,
        resolver_reads_models_dev,
        wire_override,
    )
}

/// #6705: OpenCode Zen is the one model-aware route whose protocol roster
/// comes from the Models.dev snapshot, and only the provider-lake resolver
/// carries that snapshot. There, an unproven Zen model may simply be newer than
/// the loaded catalog, so name the refresh that can prove it. Anywhere else a
/// refresh cannot change the answer and is not offered.
fn route_error_text(
    provider: ProviderKind,
    resolver_reads_models_dev: bool,
    err: &RouteError,
) -> String {
    let text = err.to_string();
    match err {
        RouteError::UnsupportedModelProtocol { endpoint_key, .. }
            if resolver_reads_models_dev
                && provider == ProviderKind::OpencodeZen
                && endpoint_key == "unproven" =>
        {
            format!("{text}, or refresh the Models.dev catalog with `codewhale models --update`")
        }
        _ => text,
    }
}

#[allow(clippy::too_many_arguments)]
fn resolve_route_candidate_with_catalog_resolver(
    provider: ProviderKind,
    model_selector: Option<&str>,
    saved_provider_model: Option<&str>,
    base_url_override: Option<String>,
    context_window_override: Option<u32>,
    model_context_windows: Option<&BTreeMap<String, u32>>,
    provider_reported_context: Option<ProviderReportedKimiCodeContext>,
    codex_roster: Option<&CodexModelRoster>,
    resolver: &RouteResolver,
    endpoint_catalog_authoritative: bool,
    resolver_reads_models_dev: bool,
    wire_override: Option<codewhale_config::provider::WireFormat>,
) -> Result<RouteCandidateResolution, String> {
    let effective_base_url = base_url_override
        .as_deref()
        .unwrap_or_else(|| provider.provider().default_base_url());
    if let Some(model) = model_selector.or(saved_provider_model) {
        validate_kimi_code_api_model_id(provider, effective_base_url, model)?;
    }
    let base_request = RouteRequest {
        explicit_provider: Some(provider),
        model_selector: model_selector.map(|model| LogicalModelRef::from(model.to_string())),
        saved_provider_model: saved_provider_model
            .map(|model| WireModelId::from(model.to_string())),
        base_url_override,
        limit_overrides: Vec::new(),
        wire_override,
    };
    // First pass: resolve the route without overrides to learn the effective
    // endpoint, wire model id, and catalog limits. Candidates are immutable, so
    // limit adjustments are planned from this read-only resolution and then
    // requested through `RouteRequest::limit_overrides` on a second pass; the
    // resolver applies them BEFORE minting the final candidate and records
    // their provenance on it.
    let resolve = |request: &RouteRequest| {
        if endpoint_catalog_authoritative {
            resolver.resolve_with_endpoint_catalog_authority(request)
        } else {
            resolver.resolve(request)
        }
    };
    let route_error = |err: RouteError| route_error_text(provider, resolver_reads_models_dev, &err);
    let resolved = resolve(&base_request).map_err(route_error)?;
    let plan = plan_limit_overrides(
        provider,
        &resolved,
        context_window_override,
        model_context_windows,
        provider_reported_context,
        codex_roster,
    );
    let candidate = if plan.overrides.is_empty() {
        resolved
    } else {
        resolve(&RouteRequest {
            limit_overrides: plan.overrides,
            wire_override: base_request.wire_override,
            ..base_request
        })
        .map_err(route_error)?
    };
    Ok(RouteCandidateResolution {
        candidate,
        context_window: plan.context_window,
    })
}

/// The sourced limit overrides a route needs, plus the context-window receipt
/// describing the effective context value they produce.
struct LimitOverridePlan {
    overrides: Vec<SourcedLimitOverride>,
    context_window: ContextWindowResolution,
}

/// Plan the limit overrides for a resolved route.
///
/// Precedence (unchanged from the previous post-hoc mutation order):
/// provider-scoped roster/API corrections and exact-route documented output
/// facts first, then operator-configured context, then fresh route-scoped
/// provider-reported context, then the membership-plan safe floor, then
/// catalog data, then the conservative fallback.
fn plan_limit_overrides(
    provider: ProviderKind,
    resolved: &ReadyRouteCandidate,
    context_window_override: Option<u32>,
    model_context_windows: Option<&BTreeMap<String, u32>>,
    provider_reported_context: Option<ProviderReportedKimiCodeContext>,
    codex_roster: Option<&CodexModelRoster>,
) -> LimitOverridePlan {
    let mut overrides = Vec::new();
    let declared_field = |field| {
        resolved
            .applied_limit_overrides()
            .iter()
            .rev()
            .find(|entry| entry.field == field)
            .is_some_and(|entry| entry.source == OverrideSource::UserModelMetadata)
    };
    // An exact wire-id hit in `model_context_windows` is a sharper operator
    // declaration than the provider default, so it wins (#6108).
    let model_configured = model_context_windows
        .and_then(|table| table.get(resolved.wire_model_id().as_str()).copied())
        .filter(|window| *window > 0);
    let configured =
        model_configured.or_else(|| context_window_override.filter(|window| *window > 0));
    let mut effective_context = resolved.limits().context_tokens;
    if !declared_field(LimitField::OutputTokens)
        && is_exact_direct_moonshot_k3_route(
            provider,
            &resolved.endpoint().base_url,
            resolved.wire_model_id().as_str(),
        )
    {
        overrides.push(SourcedLimitOverride {
            field: LimitField::OutputTokens,
            value: Some(u64::from(DIRECT_KIMI_K3_MAX_OUTPUT_TOKENS)),
            source: OverrideSource::DocumentedRouteOutputMaximum,
        });
    }
    if provider == ProviderKind::OpenaiCodex {
        // Models.dev describes the public API offering, not the account-scoped
        // ChatGPT OAuth route. Strip API-only limits, then carry the fresh
        // Codex roster's per-model context into every runtime consumer.
        overrides.push(SourcedLimitOverride {
            field: LimitField::InputTokens,
            value: None,
            source: OverrideSource::CodexPublicApiLimitStrip,
        });
        overrides.push(SourcedLimitOverride {
            field: LimitField::OutputTokens,
            value: None,
            source: OverrideSource::CodexPublicApiLimitStrip,
        });
        if configured.is_none() {
            let roster_context = codex_roster
                .filter(|roster| roster.freshness == CodexModelCacheFreshness::Fresh)
                .and_then(|roster| roster.metadata_for(resolved.wire_model_id().as_str()))
                .and_then(|metadata| metadata.context_window)
                .map(u64::from);
            effective_context = roster_context;
            overrides.push(SourcedLimitOverride {
                field: LimitField::ContextTokens,
                value: roster_context,
                source: OverrideSource::CodexRosterCorrection,
            });
        }
    }

    if let Some(context_window) = configured {
        let per_model = model_configured.is_some();
        overrides.push(SourcedLimitOverride {
            field: LimitField::ContextTokens,
            value: Some(u64::from(context_window)),
            source: if per_model {
                OverrideSource::UserModelContextWindow
            } else {
                OverrideSource::UserContextWindow
            },
        });
        return LimitOverridePlan {
            overrides,
            context_window: ContextWindowResolution {
                tokens: context_window,
                source: if per_model {
                    ContextWindowSource::ConfiguredModel
                } else {
                    ContextWindowSource::Configured
                },
            },
        };
    }

    // Exact operator metadata wins over inferred/catalog/provider-family facts.
    // A missing field remains unknown, with only the conservative budget floor.
    if declared_field(LimitField::ContextTokens) {
        let context_window = effective_context
            .and_then(|tokens| u32::try_from(tokens).ok())
            .map(|tokens| ContextWindowResolution {
                tokens,
                source: ContextWindowSource::UserDeclared,
            })
            .unwrap_or(ContextWindowResolution {
                tokens: 128_000,
                source: ContextWindowSource::Fallback,
            });
        return LimitOverridePlan {
            overrides,
            context_window,
        };
    }

    let is_exact_kimi_code_k3 = is_exact_kimi_code_bare_k3_route(
        provider,
        &resolved.endpoint().base_url,
        resolved.wire_model_id().as_str(),
    );
    let now = Utc::now();
    if is_exact_kimi_code_k3
        && provider_reported_context.is_some_and(|reported| {
            reported.context_tokens == 1_048_576
                && reported.observed_at <= now
                && now.signed_duration_since(reported.observed_at)
                    <= Duration::hours(KIMI_CODE_REPORTED_CONTEXT_MAX_AGE_HOURS)
        })
    {
        let reported = provider_reported_context.expect("checked above");
        overrides.push(SourcedLimitOverride {
            field: LimitField::ContextTokens,
            value: Some(u64::from(reported.context_tokens)),
            source: OverrideSource::ProviderReportedContextWindow,
        });
        return LimitOverridePlan {
            overrides,
            context_window: ContextWindowResolution {
                tokens: reported.context_tokens,
                source: ContextWindowSource::ProviderReported,
            },
        };
    }

    // Kimi Code's bare `k3` is a membership-plan route, not an alias for
    // Moonshot's public `kimi-k3` catalog entry.  The safe all-plan floor is
    // the route's next precedence after an explicit config or fresh, scoped
    // provider report.
    if is_exact_kimi_code_k3 {
        overrides.push(SourcedLimitOverride {
            field: LimitField::ContextTokens,
            value: Some(u64::from(KIMI_CODE_K3_CONTEXT_WINDOW_TOKENS)),
            source: OverrideSource::MembershipPlanSafeFloor,
        });
        return LimitOverridePlan {
            overrides,
            context_window: ContextWindowResolution {
                tokens: KIMI_CODE_K3_CONTEXT_WINDOW_TOKENS,
                source: ContextWindowSource::StaticKimiCodeSafeFloor,
            },
        };
    }

    if let Some(tokens) = effective_context.and_then(|tokens| u32::try_from(tokens).ok()) {
        return LimitOverridePlan {
            overrides,
            context_window: ContextWindowResolution {
                tokens,
                source: ContextWindowSource::Catalog,
            },
        };
    }

    let fallback_tokens =
        crate::provider_lake::provider_capability(provider, resolved.wire_model_id().as_str())
            .context_window;
    LimitOverridePlan {
        overrides,
        context_window: ContextWindowResolution {
            tokens: fallback_tokens,
            source: classify_capability_fallback_window(
                resolved.wire_model_id().as_str(),
                fallback_tokens,
            ),
        },
    }
}

#[cfg(any(test, feature = "test-support"))]
pub fn resolve_runtime_route(
    config: &Config,
    provider: ProviderKind,
    model_selector: Option<&str>,
) -> Result<ResolvedRuntimeRoute, String> {
    let identity = if config
        .active_provider_identity()
        .is_ok_and(|active| active.provider == provider)
    {
        config.active_provider_identity()?
    } else {
        config
            .resolve_persisted_provider_identity(Some(provider.as_str()), Some(provider.as_str()))?
    };
    resolve_runtime_route_for_identity(config, &identity, model_selector)
}

/// Resolve one persisted/live identity into a scoped runtime config and route
/// candidate. Identity is revalidated against the live registry before any
/// endpoint, model, credential, or client material is read.
pub fn resolve_runtime_route_for_identity(
    config: &Config,
    identity: &ProviderIdentity,
    model_selector: Option<&str>,
) -> Result<ResolvedRuntimeRoute, String> {
    if identity.provider == ProviderKind::Antigravity {
        return Err(codewhale_config::LEGACY_ANTIGRAVITY_TOMBSTONE_MESSAGE.to_string());
    }
    config.verify_provider_identity(identity)?;
    let original_identity = identity;
    let provider = identity.provider;
    let mut route_config = prepared_route_config(config, identity, model_selector)?;
    let identity = route_config.active_provider_identity()?;
    // The operator's effective default for the active provider is the
    // route's default too; mirror `provider_default_model` precedence so a
    // configured choice is not displaced by the provider catalog's default
    // (deepseek-flash). `auto` stays the resolver's sentinel.
    let configured_default = (config
        .active_provider_identity()
        .is_ok_and(|active| active == *original_identity)
        && config.default_text_model.is_some())
    .then(|| config.default_model())
    .filter(|model| {
        let model = model.trim();
        !model.is_empty() && !model.eq_ignore_ascii_case("auto")
    });
    let saved_provider_model =
        configured_model_for_route(&route_config, &identity).or(configured_default.as_deref());
    // #5034: with no explicit selector and no saved model, a Codex route
    // would fall back to the resolver's static seed offering. Prefer the
    // live Codex roster head so a provider switch lands on the current
    // flagship model; a missing/stale roster keeps the seed offering.
    let codex_roster =
        (provider == ProviderKind::OpenaiCodex).then(|| model_roster_for(&route_config));
    let roster_preferred = (provider == ProviderKind::OpenaiCodex
        && model_selector.is_none()
        && saved_provider_model.is_none())
    .then(|| {
        codex_roster
            .as_ref()
            .and_then(CodexModelRoster::preferred_model_id)
            .map(str::to_string)
    })
    .flatten();
    let model_selector = model_selector.or(roster_preferred.as_deref());
    let base_url = route_config.active_route_base_url();
    // A named `[providers.<name>]` table's `wire` dialect rides the resolved
    // identity's scoped config into the resolver, so the minted candidate is
    // wire-true for custom tables instead of Custom's backward-compatible
    // static Chat policy (`wire_dialect_override` degrades unknown values to
    // that same policy).
    let wire_override = (provider == ProviderKind::Custom)
        .then(|| {
            codewhale_config::provider::wire_dialect_override(
                identity.key.as_str(),
                route_config.provider_wire_dialect(&identity),
            )
        })
        .flatten();
    // Every refreshed provider shares the same exact identity/endpoint gate.
    // Codex keeps its separate authenticated account roster and protocol seam.
    let resolution = if provider != ProviderKind::OpenaiCodex {
        let status = crate::provider_catalog_live::status_for_route(
            provider,
            identity.key.as_str(),
            &base_url,
        );
        let mut catalog = crate::provider_lake::runtime_catalog_resolver_for_identity(
            provider,
            Some(identity.key.as_str()),
            &base_url,
            status,
        );
        catalog.resolver = catalog.resolver.with_configured_models(
            route_config.custom_models.as_deref().unwrap_or_default(),
            identity.key.as_str(),
            provider,
            &base_url,
        );
        // Local Ollama's placeholder is never an executable model. Resolve
        // an unset/auto/placeholder selection from this endpoint's fresh roster,
        // while preserving an explicit or saved real tag verbatim.
        let needs_local_default = provider == ProviderKind::Ollama
            && model_selector.or(saved_provider_model).is_none_or(|model| {
                model.trim().eq_ignore_ascii_case("auto")
                    || codewhale_runtime::config::is_unresolved_local_ollama_model(model)
            });
        if needs_local_default && !catalog.endpoint_catalog_authoritative {
            return Err(
                "Local Ollama has no fresh model catalog for this endpoint; select an explicit model or refresh its catalog."
                    .to_string(),
            );
        }
        let cloud_default = (model_selector.is_none()
            && saved_provider_model.is_none()
            && !catalog.endpoint_catalog_authoritative)
            .then(|| {
                codewhale_config::cloud_facts::cloud_default_model_for_route(provider, &base_url)
                    .map(|(model, _)| model)
            })
            .flatten();
        resolve_route_candidate_with_catalog_resolver(
            provider,
            model_selector
                .or(cloud_default.as_deref())
                .filter(|_| !needs_local_default),
            saved_provider_model.filter(|_| !needs_local_default),
            Some(base_url),
            route_config.context_window_for_provider_config(&identity),
            route_config.model_context_windows_for(&identity),
            None,
            None,
            &catalog.resolver,
            catalog.endpoint_catalog_authoritative,
            true,
            wire_override,
        )?
    } else {
        resolve_route_candidate_with_catalog_resolver(
            provider,
            model_selector,
            saved_provider_model,
            Some(base_url),
            route_config.context_window_for_provider_config(&identity),
            route_config.model_context_windows_for(&identity),
            None,
            codex_roster.as_ref(),
            &RouteResolver::new(),
            false,
            false,
            wire_override,
        )?
    };
    let candidate = resolution.candidate;
    let model = candidate.wire_model_id().as_str().to_string();
    if provider == ProviderKind::Ollama
        && codewhale_runtime::config::is_unresolved_local_ollama_model(&model)
    {
        return Err("Local Ollama did not report an executable default model.".to_string());
    }
    set_model_for_route(&mut route_config, &identity, &model)?;
    let identity = route_config.active_provider_identity()?;

    Ok(ResolvedRuntimeRoute {
        identity,
        candidate,
        config: Box::new(route_config),
        model,
        context_window: resolution.context_window,
        preflighted_client: None,
    })
}

fn prepared_route_config(
    config: &Config,
    identity: &ProviderIdentity,
    model_selector: Option<&str>,
) -> Result<Config, String> {
    config.verify_provider_identity(identity)?;
    let mut route_config = config.clone();
    route_config.scope_to_provider_identity(identity)?;
    if let Some(model) = model_selector {
        set_model_for_route(&mut route_config, identity, model)?;
    }
    Ok(route_config)
}

fn configured_model_for_route<'a>(
    config: &'a Config,
    identity: &ProviderIdentity,
) -> Option<&'a str> {
    config
        .provider_config_for(identity)
        .and_then(|entry| entry.model.as_deref())
}

fn set_model_for_route(
    config: &mut Config,
    identity: &ProviderIdentity,
    model: &str,
) -> Result<(), String> {
    config
        .set_provider_model_override(identity, Some(model.to_string()))
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use codewhale_config::descriptors::defaults::{DEFAULT_TEXT_MODEL, DEFAULT_ZAI_MODEL};
    use codewhale_runtime::config::{ProviderConfig, ProvidersConfig};

    #[test]
    fn configured_model_limits_precede_provider_defaults() {
        let _env = crate::test_support::lock_test_env();
        let _catalog = crate::provider_lake::lock_live_snapshot();
        for (provider, identity, base, model) in [
            (
                ProviderKind::Moonshot,
                "moonshot",
                "https://api.moonshot.ai/v1",
                "kimi-k3",
            ),
            (
                ProviderKind::Moonshot,
                "moonshot",
                "https://api.kimi.com/coding/v1",
                "k3",
            ),
            (
                ProviderKind::Deepseek,
                "deepseek-cn",
                "https://models.example.test/v1",
                "deepseek-v4.1-flash-expires-on-0910",
            ),
        ] {
            let mut config: Config = toml::from_str(include_str!(
                "../../config/tests/fixtures/custom_models.toml"
            ))
            .unwrap();
            config.provider = Some(identity.into());
            config.providers = None;
            config
                .set_provider_base_url_override(
                    &config.test_identity_for_kind(provider),
                    Some(base.into()),
                )
                .unwrap();
            let declaration = &mut config.custom_models.as_mut().unwrap()[0];
            declaration.provider = identity.into();
            declaration.base_url = base.into();
            declaration.id = model.into();
            let route = resolve_runtime_route(&config, provider, Some(model)).unwrap();
            assert_eq!(route.model, model);
            assert_eq!(route.candidate.limits().context_tokens, Some(96000));
            assert_eq!(route.candidate.limits().output_tokens, Some(8000));
            assert_eq!(
                route.context_window.source,
                ContextWindowSource::UserDeclared
            );
            config.custom_models.as_mut().unwrap()[0].limit = None;
            let unknown = resolve_runtime_route(&config, provider, Some(model)).unwrap();
            assert_eq!(unknown.candidate.limits().context_tokens, None);
            assert_eq!(unknown.candidate.limits().output_tokens, None);
            assert_eq!(unknown.context_window.source, ContextWindowSource::Fallback);
            let budget = crate::route_budget::route_context_budget_with_window(
                provider,
                model,
                crate::route_budget::known_route_limits(unknown.candidate.limits()),
                Some(unknown.context_window.tokens),
                0,
            )
            .unwrap();
            assert_eq!(
                budget.window_tokens,
                u64::from(unknown.context_window.tokens)
            );
        }
    }

    /// Every rung keeps its own label and round-trips through it, and only
    /// the guesses read as unverified.  Two rungs sharing a label would let a
    /// guess be displayed as evidence.
    #[test]
    fn every_context_window_rung_round_trips_its_own_label() {
        let mut seen = Vec::new();
        for source in ContextWindowSource::ALL {
            let label = source.label();
            assert!(!label.is_empty(), "{source:?} must carry a label");
            assert!(
                !seen.contains(&label),
                "{source:?} reuses the label {label}"
            );
            seen.push(label);
            assert_eq!(ContextWindowSource::from_label(label), Some(source));
            assert_eq!(
                source.is_verified(),
                !matches!(
                    source,
                    ContextWindowSource::Fallback
                        | ContextWindowSource::NameSuffixHint
                        | ContextWindowSource::UserDeclared
                ),
                "{source:?} misreports whether its window rests on route evidence"
            );
            assert_eq!(
                source.honesty_suffix(),
                if source.is_verified() {
                    ""
                } else {
                    " (unverified)"
                },
                "{source:?} must mark every guess it renders"
            );
        }
        assert_eq!(ContextWindowSource::from_label("configured "), None);
    }

    /// #5441: a window parsed from an `_Nk` model-name suffix is its own
    /// unverified rung — optimistic, unlike the conservative fallback — and
    /// any concrete fact about the route still beats it.
    #[test]
    fn name_suffix_hint_is_its_own_unverified_rung_below_catalog() {
        let resolved =
            resolve_context_window(ProviderKind::Custom, "qwen3-32b-256k", None, None, None);

        assert_eq!(resolved.tokens, 256_000);
        assert_eq!(resolved.source, ContextWindowSource::NameSuffixHint);
        assert!(!resolved.source.is_verified());
        assert_eq!(resolved.source.label(), "model-name hint");
        assert_eq!(
            resolved.source.display_label(),
            "model-name hint (unverified)"
        );

        // The ladder is positional and the hint sits below catalog data: an
        // offering that describes the same id wins.
        let offering = Some(RouteLimits {
            context_tokens: Some(131_072),
            ..RouteLimits::default()
        });
        let catalog =
            resolve_context_window(ProviderKind::Custom, "qwen3-32b-256k", offering, None, None);
        assert_eq!(catalog.tokens, 131_072);
        assert_eq!(catalog.source, ContextWindowSource::Catalog);
        assert!(catalog.source.is_verified());

        // An operator override beats both, exactly as before.
        let configured = resolve_context_window(
            ProviderKind::Custom,
            "qwen3-32b-256k",
            offering,
            Some(1_048_576),
            None,
        );
        assert_eq!(configured.source, ContextWindowSource::Configured);
    }

    /// #5239: an id nothing describes must land on the fallback rung and say
    /// so, rather than borrowing the configured rung's authority for a guess.
    #[test]
    fn unknown_model_resolves_to_the_honest_fallback_rung() {
        let resolved = resolve_context_window(
            ProviderKind::Custom,
            "private-1m-deployment-v9",
            None,
            None,
            None,
        );

        assert_eq!(resolved.source, ContextWindowSource::Fallback);
        assert_eq!(resolved.source.label(), "fallback");
        assert!(!resolved.source.is_verified());
        assert_eq!(
            resolved.tokens,
            crate::route_budget::route_context_window_tokens(
                ProviderKind::Custom,
                "private-1m-deployment-v9",
                None,
            )
        );
    }

    /// The same unknown id with an operator override is a configured 1M route,
    /// not a 128K one — and the rung must say which of the two it is.
    #[test]
    fn configured_override_outranks_offering_limits_and_the_fallback() {
        let offering = Some(RouteLimits {
            context_tokens: Some(131_072),
            ..RouteLimits::default()
        });

        for limits in [None, offering] {
            let resolved = resolve_context_window(
                ProviderKind::Custom,
                "private-1m-deployment-v9",
                limits,
                Some(1_048_576),
                None,
            );
            assert_eq!(resolved.tokens, 1_048_576);
            assert_eq!(resolved.source, ContextWindowSource::Configured);
        }

        let catalog = resolve_context_window(
            ProviderKind::Custom,
            "private-1m-deployment-v9",
            offering,
            None,
            None,
        );
        assert_eq!(catalog.tokens, 131_072);
        assert_eq!(catalog.source, ContextWindowSource::Catalog);
    }

    /// A zero or absent override is not a configuration decision; it must not
    /// promote a guess to the configured rung.
    #[test]
    fn empty_override_and_empty_offering_stay_on_the_fallback_rung() {
        for (limits, over) in [
            (None, Some(0)),
            (
                Some(RouteLimits {
                    context_tokens: Some(0),
                    ..RouteLimits::default()
                }),
                None,
            ),
        ] {
            assert_eq!(
                resolve_context_window(
                    ProviderKind::Custom,
                    "private-1m-deployment-v9",
                    limits,
                    over,
                    None,
                )
                .source,
                ContextWindowSource::Fallback
            );
        }
    }

    #[test]
    fn resolved_runtime_route_keeps_large_config_off_async_stacks() {
        assert!(
            std::mem::size_of::<ResolvedRuntimeRoute>() <= 1024,
            "resolved routes cross several async boundaries and must keep Config boxed"
        );
        assert!(
            std::mem::size_of::<ResolvedRuntimeRoute>() < std::mem::size_of::<Config>(),
            "resolved routes must remain smaller than their scoped Config payload"
        );
    }

    #[test]
    fn provider_route_preflight_missing_key_error_surfaces_reason_and_auth_step() {
        let err = anyhow::anyhow!(
            "Custom provider 'lm-studio' API key not found. Run 'codewhale auth set --provider custom'."
        );
        let formatted = format_provider_route_preflight_error(
            "lm-studio",
            "local-model",
            &err,
            RouteErrorSurface::Interactive,
        );

        assert!(formatted.starts_with("Custom provider 'lm-studio' API key not found."));
        assert!(formatted.contains("Failed to configure provider route lm-studio / local-model."));
        assert!(formatted.contains(
            "Next step: Run /auth or /provider setup lm-studio to configure credentials."
        ));
    }

    #[test]
    fn provider_route_preflight_headless_missing_key_is_one_cli_message() {
        let single = anyhow::anyhow!(
            "Custom provider 'lm-studio' API key not found. Run 'codewhale auth set --provider custom'."
        );
        let formatted = format_provider_route_preflight_error(
            "lm-studio",
            "local-model",
            &single,
            RouteErrorSurface::Headless,
        );
        assert!(!formatted.contains(".."), "{formatted}");
        assert!(!formatted.contains("/auth"), "{formatted}");
        assert!(!formatted.contains("/provider"), "{formatted}");
        assert!(!formatted.contains("Next step"), "{formatted}");

        let bare = anyhow::anyhow!("DeepSeek API key not found.");
        let formatted = format_provider_route_preflight_error(
            "deepseek",
            "deepseek-v4-flash",
            &bare,
            RouteErrorSurface::Headless,
        );
        assert_eq!(
            formatted,
            "DeepSeek API key not found. Failed to configure provider route deepseek / deepseek-v4-flash. Next step: Run `codewhale auth set --provider deepseek`."
        );

        // An OAuth access-token failure is not an `auth set` (API key) fix.
        let oauth = anyhow::anyhow!("OAuth access token is empty");
        let formatted = format_provider_route_preflight_error(
            "xai",
            "grok-4",
            &oauth,
            RouteErrorSurface::Headless,
        );
        assert!(!formatted.contains("auth set"), "{formatted}");
        assert!(formatted.contains("codewhale doctor"), "{formatted}");

        let multi = anyhow::anyhow!(
            "DeepSeek API key not found.\n\n  codewhale auth set --provider deepseek"
        );
        let formatted = format_provider_route_preflight_error(
            "deepseek",
            "deepseek-v4-flash",
            &multi,
            RouteErrorSurface::Headless,
        );
        assert!(
            formatted.contains(
                "\n  codewhale auth set --provider deepseek\nFailed to configure provider route"
            ),
            "{formatted}"
        );
    }

    #[test]
    fn provider_route_preflight_codex_oauth_errors_surface_the_right_next_step() {
        let missing = anyhow::anyhow!("OpenAI Codex OAuth credentials are unavailable.");
        let missing_formatted = format_provider_route_preflight_error(
            "openai-codex",
            "gpt-5.6-sol",
            &missing,
            RouteErrorSurface::Interactive,
        );
        assert!(missing_formatted.contains(
            "Next step: Run `codewhale auth chatgpt` or /provider setup openai-codex to Sign in with ChatGPT."
        ));

        let custom = anyhow::anyhow!(
            "ChatGPT credentials are only available on the official public API route"
        );
        let custom_formatted = format_provider_route_preflight_error(
            "openai-codex",
            "gpt-5.6-sol",
            &custom,
            RouteErrorSurface::Interactive,
        );
        assert!(custom_formatted.contains(
            "Next step: Run /provider setup openai-codex and remove its custom base URL; ChatGPT plan access only works on the official public API route."
        ));
    }

    #[test]
    fn provider_route_preflight_tls_error_surfaces_route_and_setup_step() {
        let err = anyhow::anyhow!(
            "TLS certificate verification cannot be disabled for provider custom; configure SSL_CERT_FILE with a trusted custom CA bundle instead"
        );
        let formatted = format_provider_route_preflight_error(
            "lm-studio",
            "local-model",
            &err,
            RouteErrorSurface::Interactive,
        );

        assert!(
            formatted
                .starts_with("TLS certificate verification cannot be disabled for provider custom")
        );
        assert!(formatted.contains("Failed to configure provider route lm-studio / local-model."));
        assert!(
            formatted
                .contains("Next step: Run /provider setup lm-studio to fix base URL/TLS settings.")
        );
    }

    #[test]
    fn chatgpt_route_ignores_external_context_and_drops_api_only_limits() {
        let _lock = crate::test_support::lock_test_env();
        let codex_home = tempfile::tempdir().expect("Codex home");
        let _home = crate::test_support::EnvVarGuard::set("CODEX_HOME", codex_home.path());
        std::fs::write(
            codex_home.path().join("models_cache.json"),
            serde_json::to_vec(&serde_json::json!({
                "fetched_at": chrono::Utc::now(),
                "models": [{
                    "slug": codewhale_config::descriptors::defaults::DEFAULT_OPENAI_CODEX_MODEL,
                    "priority": 1,
                    "context_window": 128000,
                    "supported_reasoning_levels": [{"effort": "high"}]
                }]
            }))
            .expect("serialize cache"),
        )
        .expect("write cache");

        let candidate = resolve_route_candidate(
            ProviderKind::OpenaiCodex,
            Some(codewhale_config::descriptors::defaults::DEFAULT_OPENAI_CODEX_MODEL),
            None,
            None,
            None,
            None,
        )
        .expect("Codex route");

        assert_eq!(candidate.limits().context_tokens, None);
        assert_eq!(candidate.limits().input_tokens, None);
        assert_eq!(candidate.limits().output_tokens, None);
        assert_eq!(
            crate::route_budget::route_context_window_tokens(
                ProviderKind::OpenaiCodex,
                codewhale_config::descriptors::defaults::DEFAULT_OPENAI_CODEX_MODEL,
                Some(candidate.limits()),
            ),
            128_000
        );
    }

    #[test]
    fn codex_switch_without_saved_model_prefers_fresh_roster_head() {
        // #5034: switching to openai-codex with no saved model must land on
        // the roster's current flagship, not the static seed constant.
        let _lock = crate::test_support::lock_test_env();
        let home = tempfile::tempdir().expect("Codewhale home");
        let home_path = home
            .path()
            .canonicalize()
            .expect("canonical Codewhale home");
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", &home_path);
        let mut config = codewhale_runtime::config::Config::default();
        codewhale_runtime::oauth::install_test_chatgpt_registration(&mut config)
            .expect("owned grant");
        codewhale_runtime::codex_model_roster::install_test_chatgpt_roster(
            &config,
            &[
                "gpt-test-flagship",
                codewhale_config::descriptors::defaults::DEFAULT_OPENAI_CODEX_MODEL,
            ],
        )
        .expect("account roster");
        let route = resolve_runtime_route(&config, ProviderKind::OpenaiCodex, None)
            .expect("codex route resolves");
        assert_eq!(route.model, "gpt-test-flagship");

        // An explicit selector or saved provider model still wins.
        let explicit = resolve_runtime_route(
            &config,
            ProviderKind::OpenaiCodex,
            Some(codewhale_config::descriptors::defaults::DEFAULT_OPENAI_CODEX_MODEL),
        )
        .expect("explicit codex route resolves");
        assert_eq!(
            explicit.model,
            codewhale_config::descriptors::defaults::DEFAULT_OPENAI_CODEX_MODEL
        );
    }

    #[test]
    fn opencode_go_kimi_k3_route_uses_1m_context() {
        // OpenCode Go may not own a models.dev row for kimi-k3; capability and
        // budget resolution still must use the 1M K3 contract, never the 128K
        // legacy fallback or the 131K max-output field.
        let cap = crate::provider_lake::provider_capability(ProviderKind::OpencodeGo, "kimi-k3");
        assert_eq!(cap.context_window, 1_048_576);
        assert_eq!(cap.max_output, Some(131_072));
        assert_ne!(Some(cap.context_window), cap.max_output);

        let candidate = resolve_route_candidate(
            ProviderKind::OpencodeGo,
            Some("kimi-k3"),
            None,
            None,
            None,
            None,
        )
        .expect("OpenCode Go Kimi K3 route");
        assert_eq!(candidate.wire_model_id().as_str(), "kimi-k3");
        // Prefer catalog/route limits when present; otherwise the capability
        // path above is the source of truth for picker/budget display.
        if let Some(ctx) = candidate.limits().context_tokens {
            assert_eq!(ctx, 1_048_576);
        } else {
            assert_eq!(
                crate::route_budget::route_context_window_tokens(
                    ProviderKind::OpencodeGo,
                    "kimi-k3",
                    Some(candidate.limits()),
                ),
                1_048_576
            );
        }
    }

    #[test]
    fn direct_moonshot_k3_route_uses_documented_1m_limits_with_provenance() {
        let candidate = resolve_route_candidate(
            ProviderKind::Moonshot,
            Some("kimi-k3"),
            None,
            None,
            None,
            None,
        )
        .expect("Moonshot Kimi K3 route");

        assert_eq!(candidate.wire_model_id().as_str(), "kimi-k3");
        assert_eq!(candidate.limits().context_tokens, Some(1_048_576));
        assert_eq!(candidate.limits().output_tokens, Some(1_048_576));
        assert!(candidate.applied_limit_overrides().contains(
            &codewhale_config::route::SourcedLimitOverride {
                field: codewhale_config::route::LimitField::OutputTokens,
                value: Some(1_048_576),
                source: codewhale_config::route::OverrideSource::DocumentedRouteOutputMaximum,
            }
        ));
        assert_eq!(
            crate::route_budget::route_context_window_tokens(
                ProviderKind::Moonshot,
                "kimi-k3",
                Some(candidate.limits()),
            ),
            1_048_576
        );
        assert_eq!(
            crate::route_budget::effective_max_output_tokens_for_route(
                ProviderKind::Moonshot,
                "kimi-k3",
                Some(candidate.limits()),
            ),
            65_536,
            "the documented catalogue output ceiling remains a ceiling; the safe default request must not reserve it in full"
        );
    }

    #[test]
    fn kimi_code_bare_k3_keeps_tier_safe_floor_not_legacy_128k() {
        // Bare `k3` membership context is plan-tier dependent (256K on lower
        // tiers, up to 1M on higher ones), so the static route baseline stays
        // the safe floor. Higher entitlements come from an explicit provider
        // `context_window` override — never from assuming the top tier, and
        // never from the 128K legacy default.
        let candidate = resolve_route_candidate(
            ProviderKind::Moonshot,
            Some("k3"),
            None,
            Some(codewhale_config::descriptors::defaults::DEFAULT_KIMI_CODE_BASE_URL.to_string()),
            None,
            None,
        )
        .expect("Kimi Code K3 route");

        assert_eq!(candidate.wire_model_id().as_str(), "k3");
        assert_eq!(candidate.limits().context_tokens, Some(262_144));
        // Output remains a conservative generic default because the
        // membership API does not publish a distinct maximum. Never project
        // it as context or inherit the direct-platform 1M maximum.
        assert_ne!(
            candidate.limits().context_tokens,
            candidate.limits().output_tokens
        );
        assert_eq!(
            crate::provider_lake::provider_capability(
                ProviderKind::Moonshot,
                codewhale_config::catalog::reviewed::constants::KIMI_CODE_K3_MODEL
            )
            .context_window,
            262_144
        );
        assert_ne!(candidate.limits().output_tokens, Some(1_048_576));
    }

    #[test]
    fn kimi_code_context_resolution_records_precedence_and_rejects_bad_metadata() {
        let base =
            Some(codewhale_config::descriptors::defaults::DEFAULT_KIMI_CODE_BASE_URL.to_string());
        let static_floor = resolve_route_candidate_with_context_metadata(
            ProviderKind::Moonshot,
            Some("k3"),
            None,
            base.clone(),
            None,
            None,
            None,
        )
        .expect("Kimi Code route");
        assert_eq!(static_floor.context_window.tokens, 262_144);
        assert_eq!(
            static_floor.context_window.source,
            ContextWindowSource::StaticKimiCodeSafeFloor
        );

        let configured = resolve_route_candidate_with_context_metadata(
            ProviderKind::Moonshot,
            Some("k3"),
            None,
            base.clone(),
            Some(1_048_576),
            None,
            Some(ProviderReportedKimiCodeContext {
                context_tokens: 1_048_576,
                observed_at: Utc::now(),
            }),
        )
        .expect("configured route");
        assert_eq!(configured.context_window.tokens, 1_048_576);
        assert_eq!(
            configured.context_window.source,
            ContextWindowSource::Configured
        );

        let reported = resolve_route_candidate_with_context_metadata(
            ProviderKind::Moonshot,
            Some("k3"),
            None,
            base.clone(),
            None,
            None,
            Some(ProviderReportedKimiCodeContext {
                context_tokens: 1_048_576,
                observed_at: Utc::now(),
            }),
        )
        .expect("fresh documented provider metadata");
        assert_eq!(reported.context_window.tokens, 1_048_576);
        assert_eq!(
            reported.context_window.source,
            ContextWindowSource::ProviderReported
        );

        let stale = resolve_route_candidate_with_context_metadata(
            ProviderKind::Moonshot,
            Some("k3"),
            None,
            base,
            None,
            None,
            Some(ProviderReportedKimiCodeContext {
                context_tokens: 1_048_576,
                observed_at: Utc::now() - Duration::hours(25),
            }),
        )
        .expect("stale metadata falls back safely");
        assert_eq!(
            stale.context_window.source,
            ContextWindowSource::StaticKimiCodeSafeFloor
        );

        let generic_err = resolve_route_candidate_with_context_metadata(
            ProviderKind::Moonshot,
            Some("k3"),
            None,
            Some(codewhale_config::descriptors::defaults::DEFAULT_MOONSHOT_BASE_URL.to_string()),
            None,
            None,
            Some(ProviderReportedKimiCodeContext {
                context_tokens: 1_048_576,
                observed_at: Utc::now(),
            }),
        )
        .expect_err("bare k3 is rejected on the direct Moonshot endpoint (#4687)");
        assert!(
            generic_err.contains("kimi-k3"),
            "error should guide the user to kimi-k3: {generic_err}"
        );
    }

    #[test]
    fn kimi_code_k3_context_override_wins_over_conservative_baseline() {
        let candidate = resolve_route_candidate(
            ProviderKind::Moonshot,
            Some("k3"),
            None,
            Some(codewhale_config::descriptors::defaults::DEFAULT_KIMI_CODE_BASE_URL.to_string()),
            Some(1_048_576),
            None,
        )
        .expect("Kimi Code K3 route");

        assert_eq!(
            candidate.wire_model_id().as_str(),
            codewhale_config::catalog::reviewed::constants::KIMI_CODE_K3_MODEL,
            "the 1M entitlement changes limits, never the provider wire id"
        );
        assert!(codewhale_runtime::config::is_exact_kimi_code_bare_k3_route(
            ProviderKind::Moonshot,
            &candidate.endpoint().base_url,
            candidate.wire_model_id().as_str(),
        ));
        assert_eq!(candidate.limits().context_tokens, Some(1_048_576));
    }

    #[test]
    fn model_context_windows_exact_hit_wins_over_provider_default() {
        let windows = BTreeMap::from([
            ("kimi-k3".to_string(), 512_000u32),
            ("MiniMaxAI/MiniMax-M2.5".to_string(), 204_800),
        ]);
        let resolution = resolve_route_candidate_with_context_metadata(
            ProviderKind::Moonshot,
            Some("kimi-k3"),
            None,
            None,
            Some(1_048_576),
            Some(&windows),
            None,
        )
        .expect("Moonshot route with a per-model override");

        assert_eq!(resolution.context_window.tokens, 512_000);
        assert_eq!(
            resolution.context_window.source,
            ContextWindowSource::ConfiguredModel
        );
        assert_eq!(resolution.candidate.limits().context_tokens, Some(512_000));
        assert!(
            resolution
                .candidate
                .applied_limit_overrides()
                .iter()
                .any(|entry| entry.field == LimitField::ContextTokens
                    && entry.value == Some(512_000)
                    && entry.source == OverrideSource::UserModelContextWindow),
            "candidate provenance must name the per-model override source"
        );
    }

    #[test]
    fn model_context_windows_miss_falls_back_to_provider_default() {
        let windows = BTreeMap::from([("unrelated-model".to_string(), 512_000u32)]);
        let resolution = resolve_route_candidate_with_context_metadata(
            ProviderKind::Moonshot,
            Some("kimi-k3"),
            None,
            None,
            Some(1_048_576),
            Some(&windows),
            None,
        )
        .expect("provider default applies when no model key matches");

        assert_eq!(resolution.context_window.tokens, 1_048_576);
        assert_eq!(
            resolution.context_window.source,
            ContextWindowSource::Configured
        );
        assert!(
            resolution
                .candidate
                .applied_limit_overrides()
                .iter()
                .any(|entry| entry.field == LimitField::ContextTokens
                    && entry.source == OverrideSource::UserContextWindow),
            "a table miss must stay on the provider-level provenance"
        );
    }

    #[test]
    fn resolve_context_window_per_model_rung_precedes_provider_override() {
        let windows = BTreeMap::from([("qwen3-32b-256k".to_string(), 100_000u32)]);
        let hit = resolve_context_window(
            ProviderKind::Custom,
            "qwen3-32b-256k",
            None,
            Some(999_999),
            Some(&windows),
        );
        assert_eq!(hit.tokens, 100_000);
        assert_eq!(hit.source, ContextWindowSource::ConfiguredModel);
        assert_eq!(hit.source.label(), "configured (per-model)");

        // A miss on the exact wire id falls through to the provider default.
        let miss = resolve_context_window(
            ProviderKind::Custom,
            "unrelated-model",
            None,
            Some(999_999),
            Some(&windows),
        );
        assert_eq!(miss.tokens, 999_999);
        assert_eq!(miss.source, ContextWindowSource::Configured);

        // A zero entry is ignored, never treated as a configured window.
        let zeroed = BTreeMap::from([("qwen3-32b-256k".to_string(), 0u32)]);
        let fallback = resolve_context_window(
            ProviderKind::Custom,
            "qwen3-32b-256k",
            None,
            Some(999_999),
            Some(&zeroed),
        );
        assert_eq!(fallback.tokens, 999_999);
        assert_eq!(fallback.source, ContextWindowSource::Configured);
    }

    #[test]
    fn kimi_code_rejects_claude_only_k3_1m_alias_for_selected_and_saved_models() {
        for (selected, saved) in [(Some("k3[1m]"), None), (None, Some("k3[1m]"))] {
            let error = resolve_route_candidate(
                ProviderKind::Moonshot,
                selected,
                saved,
                Some(
                    codewhale_config::descriptors::defaults::DEFAULT_KIMI_CODE_BASE_URL.to_string(),
                ),
                None,
                None,
            )
            .expect_err("Claude Code's context hint is not a Kimi Code API model id");

            assert!(error.contains("model = \"k3\""), "{error}");
            assert!(error.contains("context_window = 1048576"), "{error}");
            assert!(error.contains("plan includes 1M context"), "{error}");
            assert!(error.contains("262144 safe default"), "{error}");
        }
    }

    #[test]
    fn k3_route_rejects_cross_paired_model_ids_and_allows_canonical_pairs() {
        use codewhale_config::catalog::reviewed::constants::{
            KIMI_CODE_K3_MODEL, MOONSHOT_KIMI_K3_MODEL,
        };
        use codewhale_config::descriptors::defaults::{
            DEFAULT_KIMI_CODE_BASE_URL, DEFAULT_MOONSHOT_BASE_URL,
        };
        use codewhale_runtime::config::{
            moonshot_k3_route_display_name, validate_kimi_code_api_model_id,
        };

        // Canonical pairs succeed.
        validate_kimi_code_api_model_id(
            ProviderKind::Moonshot,
            DEFAULT_KIMI_CODE_BASE_URL,
            KIMI_CODE_K3_MODEL,
        )
        .expect("kimi code + k3");
        validate_kimi_code_api_model_id(
            ProviderKind::Moonshot,
            DEFAULT_MOONSHOT_BASE_URL,
            MOONSHOT_KIMI_K3_MODEL,
        )
        .expect("direct + kimi-k3");

        // Trailing slash normalization still enforces.
        let err = validate_kimi_code_api_model_id(
            ProviderKind::Moonshot,
            "https://api.kimi.com/coding/v1/",
            "kimi-k3",
        )
        .expect_err("kimi code + kimi-k3");
        assert!(err.contains("k3"), "{err}");
        assert!(err.contains("kimi-k3"), "{err}");

        let err = validate_kimi_code_api_model_id(
            ProviderKind::Moonshot,
            "https://api.moonshot.ai/v1/",
            "k3",
        )
        .expect_err("direct + k3");
        assert!(err.contains("kimi-k3"), "{err}");

        // Custom gateway is not rejected for either model id.
        validate_kimi_code_api_model_id(
            ProviderKind::Moonshot,
            "https://gateway.example.com/v1",
            "k3",
        )
        .expect("custom + k3");
        validate_kimi_code_api_model_id(
            ProviderKind::Moonshot,
            "https://gateway.example.com/v1",
            "kimi-k3",
        )
        .expect("custom + kimi-k3");

        // Runtime resolve fails closed the same way.
        let err = resolve_route_candidate(
            ProviderKind::Moonshot,
            Some("kimi-k3"),
            None,
            Some(DEFAULT_KIMI_CODE_BASE_URL.to_string()),
            None,
            None,
        )
        .expect_err("resolve kimi code + kimi-k3");
        assert!(err.contains("k3"), "{err}");

        let err = resolve_route_candidate(
            ProviderKind::Moonshot,
            Some("k3"),
            None,
            Some(DEFAULT_MOONSHOT_BASE_URL.to_string()),
            None,
            None,
        )
        .expect_err("resolve direct + k3");
        assert!(err.contains("kimi-k3"), "{err}");

        assert_eq!(
            moonshot_k3_route_display_name(DEFAULT_KIMI_CODE_BASE_URL, "k3"),
            Some("Kimi Code membership / k3")
        );
        assert_eq!(
            moonshot_k3_route_display_name(DEFAULT_MOONSHOT_BASE_URL, "kimi-k3"),
            Some("Moonshot direct / kimi-k3")
        );
    }

    #[test]
    fn kimi_code_k3_baseline_does_not_leak_to_other_moonshot_routes() {
        let kimi_code_endpoint =
            Some(codewhale_config::descriptors::defaults::DEFAULT_KIMI_CODE_BASE_URL.to_string());
        let direct_moonshot = resolve_route_candidate(
            ProviderKind::Moonshot,
            Some(codewhale_config::catalog::reviewed::constants::MOONSHOT_KIMI_K3_MODEL),
            None,
            Some(codewhale_config::descriptors::defaults::DEFAULT_MOONSHOT_BASE_URL.to_string()),
            None,
            None,
        )
        .expect("direct Moonshot K3 route");
        assert_eq!(direct_moonshot.limits().context_tokens, Some(1_048_576));

        // Bare k3 on the direct platform endpoint is fail-closed (#4687).
        let generic_err = resolve_route_candidate(
            ProviderKind::Moonshot,
            Some("k3"),
            None,
            Some(codewhale_config::descriptors::defaults::DEFAULT_MOONSHOT_BASE_URL.to_string()),
            None,
            None,
        )
        .expect_err("bare k3 on direct Moonshot must fail closed");
        assert!(generic_err.contains("kimi-k3"), "{generic_err}");

        // A non-K3 direct model must not inherit the Kimi Code 262k floor.
        let generic_moonshot = resolve_route_candidate(
            ProviderKind::Moonshot,
            Some("moonshot-v1-128k"),
            None,
            Some(codewhale_config::descriptors::defaults::DEFAULT_MOONSHOT_BASE_URL.to_string()),
            None,
            None,
        )
        .expect("generic Moonshot route");
        assert_ne!(generic_moonshot.limits().context_tokens, Some(262_144));

        let kimi_code_default = resolve_route_candidate(
            ProviderKind::Moonshot,
            Some(codewhale_config::descriptors::defaults::DEFAULT_KIMI_CODE_MODEL),
            None,
            kimi_code_endpoint,
            None,
            None,
        )
        .expect("Kimi Code default route");
        assert_ne!(kimi_code_default.limits().context_tokens, Some(262_144));
    }

    #[test]
    fn runtime_route_without_model_uses_target_provider_default() {
        let config = {
            let __cw_config_init_0_0 = Some("openrouter".to_string());
            let __cw_config_init_0_1 = Some(ProvidersConfig {
                openrouter: ProviderConfig {
                    model: Some("deepseek/deepseek-v4-pro".to_string()),
                    ..Default::default()
                },
                ..Default::default()
            });
            let mut __cw_config_init_0_value: Config = Default::default();
            __cw_config_init_0_value.provider = __cw_config_init_0_0;
            __cw_config_init_0_value.providers = __cw_config_init_0_1;
            __cw_config_init_0_value
        };

        let route = resolve_runtime_route(&config, ProviderKind::Zai, None)
            .expect("target provider default should resolve");

        assert_eq!(route.model, DEFAULT_ZAI_MODEL);
        assert_eq!(route.config.provider.as_deref(), Some("zai"));
        assert_eq!(
            route
                .config
                .providers
                .as_ref()
                .and_then(|providers| providers.zai.model.as_deref()),
            Some(DEFAULT_ZAI_MODEL)
        );
        assert_eq!(
            route
                .config
                .providers
                .as_ref()
                .and_then(|providers| providers.openrouter.model.as_deref()),
            Some("deepseek/deepseek-v4-pro")
        );
    }

    #[test]
    fn runtime_route_rejects_foreign_direct_model_before_config_snapshot() {
        let config = {
            let __cw_config_init_1_0 = Some("deepseek".to_string());
            let __cw_config_init_1_1 = Some(ProvidersConfig {
                deepseek: ProviderConfig {
                    model: Some(DEFAULT_TEXT_MODEL.to_string()),
                    ..Default::default()
                },
                ..Default::default()
            });
            let mut __cw_config_init_1_value: Config = Default::default();
            __cw_config_init_1_value.provider = __cw_config_init_1_0;
            __cw_config_init_1_value.providers = __cw_config_init_1_1;
            __cw_config_init_1_value
        };

        let err = resolve_runtime_route(&config, ProviderKind::Zai, Some("deepseek-v4-pro"))
            .expect_err("foreign direct-provider model should reject");

        assert!(err.contains("not served by direct provider zai"));
        assert_eq!(config.provider.as_deref(), Some("deepseek"));
        assert_eq!(
            config
                .providers
                .as_ref()
                .and_then(|providers| providers.zai.model.as_deref()),
            None
        );
    }

    #[test]
    fn unpinned_spawn_route_is_conservative_and_returns_exact_wire_id() {
        let err = resolve_unpinned_model_candidate(
            ProviderKind::Moonshot,
            "deepseek-v4-pro",
            ProviderKind::Moonshot.provider().default_base_url(),
        )
        .expect_err("official Moonshot cannot inherit a DeepSeek-owned pin");
        assert!(err.contains("deepseek-v4-pro"), "names model: {err}");
        assert!(err.contains("moonshot"), "names route: {err}");
        assert!(err.contains("deepseek"), "names owner: {err}");

        let openrouter = resolve_unpinned_model_candidate(
            ProviderKind::Openrouter,
            "deepseek-v4-pro",
            ProviderKind::Openrouter.provider().default_base_url(),
        )
        .expect("aggregator alias should resolve offline");
        assert_eq!(
            openrouter.wire_model_id().as_str(),
            codewhale_config::descriptors::defaults::DEFAULT_OPENROUTER_MODEL,
        );

        let vllm = resolve_unpinned_model_candidate(
            ProviderKind::Vllm,
            "deepseek-v4-pro",
            ProviderKind::Vllm.provider().default_base_url(),
        )
        .expect("local runtime model ids stay provider-authoritative");
        assert!(!vllm.wire_model_id().as_str().is_empty());

        let custom = resolve_unpinned_model_candidate(
            ProviderKind::Moonshot,
            "deepseek-v4-pro",
            "https://gateway.example.test/v1",
        )
        .expect("a custom endpoint owns its model namespace");
        assert_eq!(custom.wire_model_id().as_str(), "deepseek-v4-pro");
    }

    fn live_catalog_offering(
        provider: &str,
        model: &str,
        base_url: &str,
    ) -> codewhale_config::catalog::CatalogOffering {
        codewhale_config::catalog::CatalogOffering {
            provider: provider.to_string(),
            wire_model_id: model.to_string(),
            endpoint_key: "chat".to_string(),
            default_for_provider: true,
            limit: Some(codewhale_config::models_dev::ModelsDevLimit {
                context: Some(654_321),
                input: Some(600_000),
                output: Some(54_321),
            }),
            cost: Some(codewhale_config::models_dev::ModelsDevCost {
                input: Some(1.25),
                output: Some(3.5),
                cache_read: None,
                cache_write: None,
                ..Default::default()
            }),
            modalities: Some(codewhale_config::models_dev::ModelsDevModalities {
                input: vec!["text".to_string(), "image".to_string()],
                output: vec!["text".to_string()],
            }),
            attachment: Some(true),
            reasoning: Some(true),
            tool_call: Some(true),
            structured_output: Some(true),
            source: codewhale_config::catalog::CatalogSource::Live {
                base_url_fingerprint: codewhale_config::catalog::base_url_fingerprint(base_url),
                fetched_at: codewhale_config::catalog::now_unix(),
            },
            ..Default::default()
        }
    }

    fn assert_live_catalog_route_facts(route: &ResolvedRuntimeRoute) {
        use codewhale_config::route::{CapabilityState, PricingSku};

        assert_eq!(route.candidate.limits().context_tokens, Some(654_321));
        assert_eq!(route.candidate.limits().input_tokens, Some(600_000));
        assert_eq!(route.candidate.limits().output_tokens, Some(54_321));
        assert_eq!(route.context_window.tokens, 654_321);
        assert_eq!(route.context_window.source, ContextWindowSource::Catalog);
        let capabilities = route.candidate.capabilities();
        assert_eq!(capabilities.attachments, CapabilityState::Supported);
        assert_eq!(capabilities.image_input, CapabilityState::Supported);
        assert_eq!(capabilities.reasoning, CapabilityState::Supported);
        assert_eq!(capabilities.native_tool_calls, CapabilityState::Supported);
        assert_eq!(capabilities.structured_output, CapabilityState::Supported);
        match route.candidate.pricing() {
            Some(PricingSku::Token {
                input_per_mtok,
                output_per_mtok,
            }) => {
                assert_eq!(*input_per_mtok, Some(1.25));
                assert_eq!(*output_per_mtok, Some(3.5));
            }
            other => panic!("expected provider-live token pricing, got {other:?}"),
        }
    }

    #[test]
    fn live_only_openrouter_model_facts_reach_runtime_and_fail_closed_on_refresh_error() {
        use codewhale_config::catalog::{CatalogRefreshError, ProviderCatalogDelta};
        use codewhale_config::route::{CapabilityState, PricingSku};

        let _env = crate::test_support::lock_test_env();
        let _live = crate::provider_lake::lock_live_snapshot();
        let home = tempfile::tempdir().expect("home");
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", home.path());
        crate::provider_catalog_live::reset_cache_for_test();
        crate::provider_lake::clear_live_snapshot();

        let base_url = "https://synthetic.openrouter.invalid/api/v1";
        let model = "synthetic/live-only-openrouter-model";
        let config = {
            let __cw_config_init_2_0 = Some("openrouter".to_string());
            let __cw_config_init_2_1 = Some(ProvidersConfig {
                openrouter: ProviderConfig {
                    base_url: Some(base_url.to_string()),
                    model: Some(model.to_string()),
                    ..Default::default()
                },
                ..Default::default()
            });
            let mut __cw_config_init_2_value: Config = Default::default();
            __cw_config_init_2_value.provider = __cw_config_init_2_0;
            __cw_config_init_2_value.providers = __cw_config_init_2_1;
            __cw_config_init_2_value
        };
        let fingerprint = codewhale_config::catalog::base_url_fingerprint(base_url);
        crate::provider_catalog_live::record_success(ProviderCatalogDelta {
            provider: "openrouter".to_string(),
            base_url_fingerprint: fingerprint.clone(),
            fetched_at: codewhale_config::catalog::now_unix(),
            offerings: vec![live_catalog_offering("openrouter", model, base_url)],
        });

        let route = resolve_runtime_route(&config, ProviderKind::Openrouter, Some(model))
            .expect("live-only OpenRouter route resolves");
        assert_eq!(route.model, model);
        assert_live_catalog_route_facts(&route);

        crate::provider_catalog_live::record_failure(
            "openrouter",
            &fingerprint,
            CatalogRefreshError::Network,
        );
        let failed = resolve_runtime_route(&config, ProviderKind::Openrouter, Some(model))
            .expect("wire id remains routable after a failed refresh");
        assert!(!failed.candidate.limits().has_known_limit());
        assert_eq!(
            failed.candidate.capabilities().image_input,
            CapabilityState::Unknown
        );
        assert!(matches!(
            failed.candidate.pricing(),
            Some(PricingSku::UnknownOrStale)
        ));

        crate::provider_catalog_live::reset_cache_for_test();
        crate::provider_lake::clear_live_snapshot();
    }

    #[test]
    fn unrelated_live_roster_cannot_change_direct_model_ownership() {
        use codewhale_config::catalog::ProviderCatalogDelta;

        let _env = crate::test_support::lock_test_env();
        let _live = crate::provider_lake::lock_live_snapshot();
        let home = tempfile::tempdir().unwrap();
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", home.path());
        crate::provider_catalog_live::reset_cache_for_test();
        crate::provider_lake::clear_live_snapshot();
        let config = {
            let __cw_config_init_3_0 = Some("deepseek".into());
            let mut __cw_config_init_3_value: Config = Default::default();
            __cw_config_init_3_value.provider = __cw_config_init_3_0;
            __cw_config_init_3_value
        };
        let model = "unlisted-future-direct-model";
        let before = resolve_runtime_route(&config, ProviderKind::Deepseek, Some(model)).unwrap();
        let endpoint = "https://other-provider.catalog.invalid/v1";
        let ticket = crate::provider_catalog_live::begin_refresh_for_identity(
            ProviderKind::Telecomjs,
            "telecomjs",
            endpoint,
        );
        crate::provider_catalog_live::record_success_if_current(
            &ticket,
            ProviderCatalogDelta {
                provider: "telecomjs".into(),
                base_url_fingerprint: codewhale_config::catalog::base_url_fingerprint(endpoint),
                fetched_at: codewhale_config::catalog::now_unix(),
                offerings: vec![live_catalog_offering("telecomjs", model, endpoint)],
            },
        );
        let after = resolve_runtime_route(&config, ProviderKind::Deepseek, Some(model)).unwrap();
        assert_eq!(after.model, before.model);
        assert_eq!(
            after.candidate.endpoint().base_url,
            before.candidate.endpoint().base_url
        );
        assert_eq!(
            after.candidate.endpoint().endpoint_key,
            before.candidate.endpoint().endpoint_key
        );
        assert_eq!(
            after.candidate.endpoint().protocol,
            before.candidate.endpoint().protocol
        );
        assert_eq!(after.candidate.limits(), before.candidate.limits());
        crate::provider_catalog_live::reset_cache_for_test();
        crate::provider_lake::clear_live_snapshot();
    }

    #[test]
    fn every_refreshed_provider_uses_fresh_exact_endpoint_route_facts() {
        use codewhale_config::catalog::{CatalogRefreshError, ProviderCatalogDelta};

        let _env = crate::test_support::lock_test_env();
        let _live = crate::provider_lake::lock_live_snapshot();
        let home = tempfile::tempdir().unwrap();
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", home.path());
        crate::provider_catalog_live::reset_cache_for_test();
        crate::provider_lake::clear_live_snapshot();
        for provider in [
            ProviderKind::Ollama,
            ProviderKind::Codewhale,
            ProviderKind::Concentrate,
            ProviderKind::Telecomjs,
            ProviderKind::Edenai,
            ProviderKind::Zenmux,
        ] {
            let identity = provider.as_str();
            let endpoint = format!("https://{identity}.catalog.invalid/v1");
            let model = "synthetic-live-model";
            let mut config = {
                let __cw_config_init_4_0 = Some(identity.into());
                let mut __cw_config_init_4_value: Config = Default::default();
                __cw_config_init_4_value.provider = __cw_config_init_4_0;
                __cw_config_init_4_value
            };
            config
                .provider_config_for_mut(&config.test_identity_for_kind(provider))
                .unwrap()
                .base_url = Some(endpoint.clone());
            let ticket = crate::provider_catalog_live::begin_refresh_for_identity(
                provider, identity, &endpoint,
            );
            let fingerprint = codewhale_config::catalog::base_url_fingerprint(&endpoint);
            assert!(
                crate::provider_catalog_live::record_success_if_current(
                    &ticket,
                    ProviderCatalogDelta {
                        provider: identity.into(),
                        base_url_fingerprint: fingerprint.clone(),
                        fetched_at: codewhale_config::catalog::now_unix(),
                        offerings: vec![live_catalog_offering(identity, model, &endpoint)],
                    }
                )
                .is_some()
            );
            let route = resolve_runtime_route(&config, provider, Some(model)).unwrap();
            assert_eq!(route.candidate.endpoint().base_url, endpoint);
            assert_live_catalog_route_facts(&route);
            let default = resolve_runtime_route(&config, provider, None).unwrap();
            assert_eq!(
                default.model, model,
                "{identity} default must come from its own roster"
            );
            let mut other = config.clone();
            other
                .provider_config_for_mut(&other.test_identity_for_kind(provider))
                .unwrap()
                .base_url = Some("https://other.catalog.invalid/v1".into());
            let unowned = resolve_runtime_route(&other, provider, Some(model)).unwrap();
            assert!(
                !unowned.candidate.limits().has_known_limit(),
                "{identity} must not reuse another endpoint's limits"
            );
            crate::provider_catalog_live::record_failure_if_current(
                &ticket,
                identity,
                &fingerprint,
                CatalogRefreshError::Network,
            );
            let failed = resolve_runtime_route(&config, provider, Some(model)).unwrap();
            assert!(
                !failed.candidate.limits().has_known_limit(),
                "{identity} failed refresh must revoke executable live facts"
            );
        }
        crate::provider_catalog_live::reset_cache_for_test();
        crate::provider_lake::clear_live_snapshot();
    }

    #[test]
    fn ollama_default_requires_fresh_endpoint_tags_and_preserves_explicit_choices() {
        use codewhale_config::catalog::{CatalogRefreshError, ProviderCatalogDelta};

        let _env = crate::test_support::lock_test_env();
        let _live = crate::provider_lake::lock_live_snapshot();
        let home = tempfile::tempdir().unwrap();
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", home.path());
        crate::provider_catalog_live::reset_cache_for_test();
        crate::provider_lake::clear_live_snapshot();
        let endpoint = "http://localhost:11451/v1";
        let mut config = {
            let __cw_config_init_5_0 = Some("ollama".into());
            let mut __cw_config_init_5_value: Config = Default::default();
            __cw_config_init_5_value.provider = __cw_config_init_5_0;
            __cw_config_init_5_value
        };
        config
            .provider_config_for_mut(&config.test_identity_for_kind(ProviderKind::Ollama))
            .unwrap()
            .base_url = Some(endpoint.into());
        for selector in [None, Some("auto"), Some("unknown")] {
            assert!(resolve_runtime_route(&config, ProviderKind::Ollama, selector).is_err());
        }
        assert_eq!(
            resolve_runtime_route(&config, ProviderKind::Ollama, Some("chosen:tag"))
                .unwrap()
                .model,
            "chosen:tag"
        );
        let fingerprint = codewhale_config::catalog::base_url_fingerprint(endpoint);
        let ticket = crate::provider_catalog_live::begin_refresh_for_identity(
            ProviderKind::Ollama,
            "ollama",
            endpoint,
        );
        let offerings = ["zeta:tag", "alpha:tag"]
            .into_iter()
            .map(|model| {
                let mut row = live_catalog_offering("ollama", model, endpoint);
                row.default_for_provider = model == "alpha:tag";
                row
            })
            .collect();
        crate::provider_catalog_live::record_success_if_current(
            &ticket,
            ProviderCatalogDelta {
                provider: "ollama".into(),
                base_url_fingerprint: fingerprint.clone(),
                fetched_at: codewhale_config::catalog::now_unix(),
                offerings,
            },
        );
        for selector in [None, Some("auto"), Some("unknown")] {
            assert_eq!(
                resolve_runtime_route(&config, ProviderKind::Ollama, selector)
                    .unwrap()
                    .model,
                "alpha:tag"
            );
        }
        config
            .set_provider_model_override(
                &config.test_identity_for_kind(ProviderKind::Ollama),
                Some("saved:tag".into()),
            )
            .unwrap();
        assert_eq!(
            resolve_runtime_route(&config, ProviderKind::Ollama, None)
                .unwrap()
                .model,
            "saved:tag"
        );
        assert_eq!(
            resolve_runtime_route(&config, ProviderKind::Ollama, Some("explicit:tag"))
                .unwrap()
                .model,
            "explicit:tag"
        );
        config
            .set_provider_model_override(
                &config.test_identity_for_kind(ProviderKind::Ollama),
                Some("unknown".into()),
            )
            .unwrap();
        assert_eq!(
            resolve_runtime_route(&config, ProviderKind::Ollama, None)
                .unwrap()
                .model,
            "alpha:tag"
        );
        crate::provider_catalog_live::record_failure_if_current(
            &ticket,
            "ollama",
            &fingerprint,
            CatalogRefreshError::Network,
        );
        assert!(resolve_runtime_route(&config, ProviderKind::Ollama, None).is_err());
        assert_eq!(
            resolve_runtime_route(&config, ProviderKind::Ollama, Some("explicit:tag"))
                .unwrap()
                .model,
            "explicit:tag"
        );
        crate::provider_catalog_live::reset_cache_for_test();
        crate::provider_lake::clear_live_snapshot();
    }

    #[test]
    fn named_baseten_live_facts_reach_exact_custom_runtime_without_leaking() {
        use codewhale_config::catalog::ProviderCatalogDelta;

        let _env = crate::test_support::lock_test_env();
        let _live = crate::provider_lake::lock_live_snapshot();
        let home = tempfile::tempdir().expect("home");
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", home.path());
        crate::provider_catalog_live::reset_cache_for_test();
        crate::provider_lake::clear_live_snapshot();

        let base_url = codewhale_config::catalog::BASETEN_BASE_URL;
        let model = "synthetic-live-baseten-model";
        let mut custom = std::collections::HashMap::new();
        custom.insert(
            codewhale_config::catalog::BASETEN_PROVIDER_ID.to_string(),
            ProviderConfig {
                kind: Some("openai-compatible".to_string()),
                base_url: Some(base_url.to_string()),
                model: Some(model.to_string()),
                ..Default::default()
            },
        );
        let config = {
            let __cw_config_init_6_0 =
                Some(codewhale_config::catalog::BASETEN_PROVIDER_ID.to_string());
            let __cw_config_init_6_1 = Some(ProvidersConfig {
                custom,
                ..Default::default()
            });
            let mut __cw_config_init_6_value: Config = Default::default();
            __cw_config_init_6_value.provider = __cw_config_init_6_0;
            __cw_config_init_6_value.providers = __cw_config_init_6_1;
            __cw_config_init_6_value
        };
        crate::provider_catalog_live::record_success(ProviderCatalogDelta {
            provider: codewhale_config::catalog::BASETEN_PROVIDER_ID.to_string(),
            base_url_fingerprint: codewhale_config::catalog::base_url_fingerprint(base_url),
            fetched_at: codewhale_config::catalog::now_unix(),
            offerings: vec![live_catalog_offering(
                codewhale_config::catalog::BASETEN_PROVIDER_ID,
                model,
                base_url,
            )],
        });

        let route = resolve_runtime_route(&config, ProviderKind::Custom, Some(model))
            .expect("named Baseten route resolves");
        assert_eq!(
            route.identity.key.as_str(),
            codewhale_config::catalog::BASETEN_PROVIDER_ID
        );
        assert_eq!(route.model, model);
        assert_live_catalog_route_facts(&route);

        let alias_identity = "base-ten";
        let alias_model = "synthetic-alias-baseten-model";
        let mut alias_custom = std::collections::HashMap::new();
        alias_custom.insert(
            alias_identity.to_string(),
            ProviderConfig {
                kind: Some("openai-compatible".to_string()),
                base_url: Some(base_url.to_string()),
                model: Some(alias_model.to_string()),
                ..Default::default()
            },
        );
        let alias_config = {
            let __cw_config_init_7_0 = Some(alias_identity.to_string());
            let __cw_config_init_7_1 = Some(ProvidersConfig {
                custom: alias_custom,
                ..Default::default()
            });
            let mut __cw_config_init_7_value: Config = Default::default();
            __cw_config_init_7_value.provider = __cw_config_init_7_0;
            __cw_config_init_7_value.providers = __cw_config_init_7_1;
            __cw_config_init_7_value
        };
        crate::provider_catalog_live::record_success(ProviderCatalogDelta {
            provider: alias_identity.to_string(),
            base_url_fingerprint: codewhale_config::catalog::base_url_fingerprint(base_url),
            fetched_at: codewhale_config::catalog::now_unix(),
            offerings: vec![live_catalog_offering(alias_identity, alias_model, base_url)],
        });
        let alias_route =
            resolve_runtime_route(&alias_config, ProviderKind::Custom, Some(alias_model))
                .expect("Baseten schema alias route resolves");
        assert_eq!(alias_route.identity.key.as_str(), alias_identity);
        assert_live_catalog_route_facts(&alias_route);
        assert!(
            crate::provider_lake::catalog_offering_for_model_identity(
                ProviderKind::Custom,
                Some(codewhale_config::catalog::BASETEN_PROVIDER_ID),
                alias_model,
            )
            .is_none(),
            "a Baseten schema alias must not share another exact table's live roster"
        );

        let unrelated = custom_config("https://other-compatible.invalid/v1", model);
        let unrelated_route = resolve_runtime_route(&unrelated, ProviderKind::Custom, Some(model))
            .expect("another compatible provider remains routable");
        assert!(!unrelated_route.candidate.limits().has_known_limit());
        assert_eq!(
            unrelated_route.candidate.capabilities(),
            codewhale_config::route::RouteCapabilities::default()
        );

        crate::provider_catalog_live::reset_cache_for_test();
        crate::provider_lake::clear_live_snapshot();
    }

    fn custom_config(base_url: &str, model: &str) -> Config {
        let mut custom = std::collections::HashMap::new();
        custom.insert(
            "my_thing".to_string(),
            ProviderConfig {
                kind: Some("openai-compatible".to_string()),
                base_url: Some(base_url.to_string()),
                model: Some(model.to_string()),
                api_key_env: Some("EXAMPLE_API_KEY".to_string()),
                ..Default::default()
            },
        );
        {
            let __cw_config_init_8_0 = Some("my_thing".to_string());
            let __cw_config_init_8_1 = Some(ProvidersConfig {
                custom,
                ..Default::default()
            });
            let mut __cw_config_init_8_value: Config = Default::default();
            __cw_config_init_8_value.provider = __cw_config_init_8_0;
            __cw_config_init_8_value.providers = __cw_config_init_8_1;
            __cw_config_init_8_value
        }
    }

    #[test]
    fn custom_provider_resolves_to_custom_endpoint_and_verbatim_model() {
        use codewhale_config::route::RequestProtocol;

        let config = custom_config("https://api.example.com/v1", "vendor/custom-model-v1");
        let route = resolve_runtime_route(&config, ProviderKind::Custom, None)
            .expect("custom provider should resolve");

        // Endpoint + model come from the named table; the prefixed model id is
        // preserved verbatim as the wire id (no provider-prefix sniffing).
        assert_eq!(
            route.candidate.endpoint().base_url,
            "https://api.example.com/v1"
        );
        assert_eq!(
            route.candidate.wire_model_id().as_str(),
            "vendor/custom-model-v1"
        );
        assert_eq!(route.model, "vendor/custom-model-v1");
        assert_eq!(route.candidate.protocol(), RequestProtocol::ChatCompletions);
        // HTTPS endpoint: route is valid with no insecure-http advisory.
        assert!(route.candidate.validation().ok);
        assert!(route.candidate.validation().messages.is_empty());
        // The selected provider name is preserved (not overwritten with "custom").
        assert_eq!(route.config.provider.as_deref(), Some("my_thing"));
    }

    #[test]
    fn custom_provider_context_window_overrides_unknown_route_limit() {
        let mut custom = std::collections::HashMap::new();
        custom.insert(
            "dashscope".to_string(),
            ProviderConfig {
                kind: Some("openai-compatible".to_string()),
                base_url: Some("https://dashscope.example.com/compatible-mode/v1".to_string()),
                model: Some("qwen3.7".to_string()),
                context_window: Some(1_000_000),
                api_key_env: Some("DASHSCOPE_API_KEY".to_string()),
                ..Default::default()
            },
        );
        let config = {
            let __cw_config_init_9_0 = Some("dashscope".to_string());
            let __cw_config_init_9_1 = Some(ProvidersConfig {
                custom,
                ..Default::default()
            });
            let mut __cw_config_init_9_value: Config = Config::default();
            __cw_config_init_9_value.provider = __cw_config_init_9_0;
            __cw_config_init_9_value.providers = __cw_config_init_9_1;
            __cw_config_init_9_value
        };

        let route = resolve_runtime_route(&config, ProviderKind::Custom, None)
            .expect("custom route should resolve");

        assert_eq!(route.model, "qwen3.7");
        assert_eq!(route.candidate.limits().context_tokens, Some(1_000_000));
    }

    #[test]
    fn custom_provider_http_non_loopback_fires_insecure_advisory() {
        let config = custom_config("http://gpu.internal.example:8000/v1", "custom-model-v1");
        let route = resolve_runtime_route(&config, ProviderKind::Custom, None)
            .expect("custom http provider should resolve");

        // Advisory only: the route still validates (ok == true) but warns that
        // credentials would be sent in plaintext over a non-loopback http URL.
        assert!(route.candidate.validation().ok);
        assert!(
            route
                .candidate
                .validation()
                .messages
                .iter()
                .any(|message| message.contains("insecure http")),
            "expected insecure-http advisory, got {:?}",
            route.candidate.validation().messages
        );
        assert_eq!(
            route.candidate.endpoint().base_url,
            "http://gpu.internal.example:8000/v1"
        );
    }

    /// #6705: only the lake-backed OpenCode Zen route reads a refreshed
    /// Models.dev catalog, so only it is told to refresh.
    #[test]
    fn catalog_refresh_remedy_is_scoped_to_the_lake_backed_zen_route() {
        let unproven = |provider: &str| RouteError::UnsupportedModelProtocol {
            provider: provider.into(),
            model: "new-model".to_string(),
            endpoint_key: "unproven".to_string(),
        };
        let remedy = "codewhale models --update";
        assert!(
            route_error_text(ProviderKind::OpencodeZen, true, &unproven("opencode-zen"))
                .contains(remedy)
        );
        // OpenCode Go's roster is compiled, and a bundled-only resolver
        // never sees the refreshed catalog.
        assert!(
            !route_error_text(ProviderKind::OpencodeGo, true, &unproven("opencode-go"))
                .contains(remedy)
        );
        assert!(
            !route_error_text(ProviderKind::OpencodeZen, false, &unproven("opencode-zen"))
                .contains(remedy)
        );
        let deprecated = RouteError::UnsupportedModelProtocol {
            provider: "opencode-zen".into(),
            model: "claude-2-retired".to_string(),
            endpoint_key: "deprecated".to_string(),
        };
        let text = route_error_text(ProviderKind::OpencodeZen, true, &deprecated);
        assert!(
            text.contains("deprecated") && !text.contains(remedy),
            "{text}"
        );
    }
    #[test]
    fn owned_chatgpt_roster_context_reaches_runtime_only_and_never_unscoped_resolver() {
        let _lock = crate::test_support::lock_test_env();
        let home = tempfile::tempdir().expect("owned registration home");
        let root = home.path().canonicalize().expect("canonical owned home");
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", &root);
        let mut config = Config::default();
        codewhale_runtime::oauth::install_test_chatgpt_registration(&mut config)
            .expect("owned grant");
        let model = codewhale_config::descriptors::defaults::DEFAULT_OPENAI_CODEX_MODEL;
        codewhale_runtime::codex_model_roster::install_test_chatgpt_roster_with_metadata(
            &config,
            vec![codewhale_runtime::codex_model_roster::CodexModelMetadata {
                id: model.into(),
                display_name: None,
                context_window: Some(272_000),
                reasoning: Some(true),
                efforts: vec!["high".into()],
            }],
        )
        .expect("account-scoped roster");
        let runtime = resolve_runtime_route(&config, ProviderKind::OpenaiCodex, Some(model))
            .expect("owned runtime route");
        assert_eq!(runtime.candidate.limits().context_tokens, Some(272_000));
        assert_eq!(runtime.context_window.tokens, 272_000);
        assert_eq!(runtime.candidate.limits().input_tokens, None);
        assert_eq!(runtime.candidate.limits().output_tokens, None);
        let unscoped = resolve_route_candidate_with_context_metadata(
            ProviderKind::OpenaiCodex,
            Some(model),
            None,
            None,
            None,
            None,
            None,
        )
        .expect("unscoped descriptor");
        assert_eq!(unscoped.candidate.limits().context_tokens, None);
    }
}

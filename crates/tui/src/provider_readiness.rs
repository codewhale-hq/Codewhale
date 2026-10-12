//! Truthful, session-local provider readiness.
//!
//! Static configuration can prove that credential material exists, but not
//! that an endpoint is reachable or an OAuth token is still entitled to a
//! model. `Ready` is therefore reserved for observed success in this session.

use std::borrow::Cow;

use crate::config::{ProviderIdentity, ProviderKind};
use crate::error_taxonomy::{ErrorCategory, ErrorEnvelope};
use codewhale_config::route::{LogicalModelRef, RouteRequest, RouteResolver};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CredentialState {
    MissingKey,
    MissingLogin,
    /// Structurally valid read-only consent exists for another CLI's file,
    /// but the provider is not active so no read capability has been minted.
    ExternalConsent,
    Saved,
    ImportedToken,
    NoAuth,
    Local,
    Legacy,
}

/// Credential route whose observed health may be reused. A provider can
/// expose more than one auth route (notably xAI and Moonshot), so provider id
/// alone is not a safe cache key: a successful API-key request must not make a
/// newly selected imported-token or OAuth route appear verified.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProviderAuthClass {
    ApiKey,
    OAuth,
    ImportedToken,
    NoAuth,
    Local,
    Legacy,
}

/// Exact route whose observed health may be reused. This deliberately keeps
/// custom provider id, endpoint, model, and auth class together: success on
/// one private endpoint or model entitlement is not evidence for another.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProviderRouteIdentity {
    identity: ProviderIdentity,
    endpoint: String,
    model: String,
    auth_class: ProviderAuthClass,
    credential_generation: Option<crate::route_receipt::CredentialGeneration>,
}

pub(crate) fn route_identity_for_model(
    config: &crate::config::Config,
    identity: &ProviderIdentity,
    model: &str,
) -> ProviderRouteIdentity {
    let endpoint = crate::route_receipt::endpoint_identity(&config.base_url_for_route(identity));
    ProviderRouteIdentity {
        identity: identity.clone(),
        endpoint,
        model: model.trim().to_string(),
        auth_class: auth_class_for_provider(config, identity),
        credential_generation: config.readonly_health_credential_generation(identity),
    }
}

pub(crate) fn auth_class_for_provider(
    config: &crate::config::Config,
    identity: &ProviderIdentity,
) -> ProviderAuthClass {
    let provider = identity.provider;
    if config.verify_provider_identity(identity).is_err() {
        return ProviderAuthClass::Legacy;
    }
    let auth_mode = config.auth_mode_for_provider(identity);
    if provider == ProviderKind::Anthropic && auth_mode.as_deref() == Some("oauth") {
        return ProviderAuthClass::OAuth;
    }
    if provider == ProviderKind::Custom
        && config
            .provider_config_for(identity)
            .is_some_and(|entry| entry.oauth.is_some())
    {
        return ProviderAuthClass::OAuth;
    }
    if crate::config::auth_mode_disables_api_key(auth_mode.as_deref()) {
        return ProviderAuthClass::NoAuth;
    }
    let official_endpoint = !config.provider_uses_custom_endpoint(identity);
    if provider == ProviderKind::OpenaiCodex && official_endpoint {
        return ProviderAuthClass::OAuth;
    }
    if provider == ProviderKind::Moonshot
        && official_endpoint
        && auth_mode
            .as_deref()
            .is_some_and(crate::config::auth_mode_uses_kimi_imported_token)
    {
        return ProviderAuthClass::ImportedToken;
    }
    if provider == ProviderKind::Xai
        && official_endpoint
        && auth_mode
            .as_deref()
            .is_some_and(crate::oauth::auth_mode_uses_xai_oauth)
    {
        return ProviderAuthClass::OAuth;
    }
    match credential_state_for_provider(config, identity) {
        CredentialState::NoAuth => ProviderAuthClass::NoAuth,
        CredentialState::Local => ProviderAuthClass::Local,
        CredentialState::Legacy => ProviderAuthClass::Legacy,
        CredentialState::ExternalConsent => ProviderAuthClass::OAuth,
        _ => ProviderAuthClass::ApiKey,
    }
}

pub(crate) fn credential_state_for_provider(
    config: &crate::config::Config,
    identity: &ProviderIdentity,
) -> CredentialState {
    let provider = identity.provider;
    if provider == ProviderKind::Antigravity {
        return CredentialState::Legacy;
    }
    if config.verify_provider_identity(identity).is_err() {
        return CredentialState::MissingKey;
    }
    let auth_mode = config.auth_mode_for_provider(identity);
    if provider == ProviderKind::Anthropic && auth_mode.as_deref() == Some("oauth") {
        return if matches!(
            config.base_url_for_route(identity).trim_end_matches('/'),
            "https://api.anthropic.com" | "https://api.anthropic.com/v1"
        ) && crate::oauth::credentials_valid(crate::oauth::OAuthProvider::Claude, config)
        {
            CredentialState::Saved
        } else {
            CredentialState::MissingLogin
        };
    }
    // Plugin OAuth takes precedence over local/keyless and legacy custom
    // classifications. Diagnostics only read the bound credential; they never
    // refresh, migrate storage or turn a missing login into API-key fallback.
    if provider == ProviderKind::Custom
        && config
            .provider_config_for(identity)
            .is_some_and(|entry| entry.oauth.is_some())
    {
        let stored = config.provider_config_for(identity).is_some_and(|entry| {
            let Some(authority) = entry.plugin_authority.as_ref() else {
                return false;
            };
            if crate::plugins::registry::verify_plugin_component_authority(
                authority,
                crate::plugins::activation::PluginActivationCapability::Providers,
            )
            .is_err()
            {
                return false;
            }
            crate::oauth::plugin_oauth_credentials_present(
                identity.key.as_str(),
                &config.base_url_for_route(identity),
                entry.oauth.as_ref().unwrap(),
            )
            .unwrap_or(false)
        });
        return if config.auth_mode_for_provider(identity).as_deref() == Some("oauth") && stored {
            CredentialState::Saved
        } else {
            CredentialState::MissingLogin
        };
    }
    if crate::config::auth_mode_disables_api_key(auth_mode.as_deref()) {
        return CredentialState::NoAuth;
    }
    let api_key_required = crate::config::auth_mode_requires_api_key(auth_mode.as_deref());
    let official_endpoint = !config.provider_uses_custom_endpoint(identity);

    // A built-in provider can intentionally target a local OpenAI-compatible
    // runtime. That route is keyless unless the operator explicitly declares
    // an API-key auth contract. Classify it before provider-specific hosted
    // branches (including the DeepSeek-CN compatibility alias) so readiness
    // and cache identity describe the effective endpoint, not just the
    // provider enum.
    if config
        .active_provider_identity()
        .is_ok_and(|active| active == *identity)
        && !official_endpoint
        && crate::config::base_url_uses_local_host(&config.active_route_base_url())
    {
        return if api_key_required {
            if crate::config::has_api_key_for(config, identity) {
                CredentialState::Saved
            } else {
                CredentialState::MissingKey
            }
        } else {
            CredentialState::Local
        };
    }

    if provider == ProviderKind::Antigravity {
        return CredentialState::Legacy;
    }
    if provider == ProviderKind::Custom {
        let Some(configured) = config.provider_config_for(identity) else {
            return CredentialState::MissingKey;
        };
        let auth_optional = configured
            .base_url
            .as_deref()
            .is_some_and(crate::config::base_url_uses_local_host)
            && !api_key_required;
        if auth_optional {
            return CredentialState::Local;
        }
        // The literal `custom` route (older top-level shape, now
        // `[providers.custom]`) also owns the generic `custom` secret slot.
        let has_auth = (config
            .active_provider_identity()
            .is_ok_and(|active| active == *identity)
            && crate::config::explicit_cli_api_key_override().is_some())
            || (config.selects_literal_custom_provider()
                && crate::config::has_api_key_for(config, identity))
            || configured.api_key.as_deref().is_some_and(|value| {
                crate::config::classify_config_api_key_value(value)
                    == crate::config::ConfigApiKeyValueKind::Literal
            })
            || configured
                .api_key_env
                .as_deref()
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .is_some_and(|name| {
                    std::env::var(name).is_ok_and(|value| !value.trim().is_empty())
                });
        return if has_auth {
            CredentialState::Saved
        } else {
            CredentialState::MissingKey
        };
    }
    if crate::config::provider_route_is_keyless_self_hosted(
        provider,
        &config.base_url_for_route(identity),
    ) {
        return if api_key_required {
            if crate::config::has_api_key_for(config, identity) {
                CredentialState::Saved
            } else {
                CredentialState::MissingKey
            }
        } else {
            CredentialState::Local
        };
    }

    let uses_kimi_imported_token = provider == ProviderKind::Moonshot
        && official_endpoint
        && auth_mode
            .as_deref()
            .is_some_and(crate::config::auth_mode_uses_kimi_imported_token);
    if uses_kimi_imported_token {
        // Kimi remains API-key-only until Codewhale has its own registered
        // OAuth client identity. Never inspect Kimi CLI storage here.
        return CredentialState::MissingKey;
    }
    if provider == ProviderKind::OpenaiCodex && official_endpoint {
        return if crate::config::has_api_key_for(config, identity) {
            CredentialState::Saved
        } else if config
            .active_provider_identity()
            .is_ok_and(|active| active != *identity)
            && config.external_credential_read_consent_configured(
                identity,
                codewhale_config::ExternalCredentialSource::CodexCli,
            )
        {
            CredentialState::ExternalConsent
        } else {
            CredentialState::MissingLogin
        };
    }
    let xai_oauth_selected = provider == ProviderKind::Xai
        && official_endpoint
        && auth_mode
            .as_deref()
            .is_some_and(crate::oauth::auth_mode_uses_xai_oauth);
    if xai_oauth_selected {
        // #5772: `Saved` means a credential was actually found. A surviving
        // consent record whose file is gone or expired resolves below as
        // `ExternalConsent` (dormant) or `MissingLogin`, never as `Saved`.
        return if crate::oauth::credentials_valid(crate::oauth::OAuthProvider::Xai, config)
            || explicit_provider_credential_present(config, identity)
        {
            CredentialState::Saved
        } else if config
            .active_provider_identity()
            .is_ok_and(|active| active != *identity)
            && config.external_credential_read_consent_configured(
                identity,
                codewhale_config::ExternalCredentialSource::GrokCli,
            )
        {
            CredentialState::ExternalConsent
        } else {
            CredentialState::MissingLogin
        };
    }
    if provider == ProviderKind::Xai && explicit_provider_credential_present(config, identity) {
        return CredentialState::Saved;
    }
    if provider == ProviderKind::Xai {
        return CredentialState::MissingKey;
    }

    if crate::config::has_api_key_for(config, identity) {
        CredentialState::Saved
    } else if matches!(
        provider,
        ProviderKind::Deepseek | ProviderKind::DeepseekAnthropic
    ) && official_endpoint
        && config
            .active_provider_identity()
            .is_ok_and(|active| active != *identity)
        && config.external_credential_read_consent_configured(
            identity,
            codewhale_config::ExternalCredentialSource::DshCli,
        )
    {
        CredentialState::ExternalConsent
    } else {
        CredentialState::MissingKey
    }
}

fn explicit_provider_credential_present(
    config: &crate::config::Config,
    identity: &ProviderIdentity,
) -> bool {
    let provider = identity.provider;
    if config.verify_provider_identity(identity).is_err() {
        return false;
    }
    (config
        .active_provider_identity()
        .is_ok_and(|active| active == *identity)
        && crate::config::explicit_cli_api_key_override().is_some())
        || (!config.provider_uses_custom_endpoint(identity)
            && provider
                .provider()
                .env_vars()
                .iter()
                .any(|name| std::env::var(name).is_ok_and(|value| !value.trim().is_empty())))
        || (config.config_credentials_are_bound_to_provider_endpoint(identity)
            && config.provider_config_for(identity).is_some_and(|entry| {
                entry.api_key.as_deref().is_some_and(|value| {
                    crate::config::classify_config_api_key_value(value)
                        == crate::config::ConfigApiKeyValueKind::Literal
                }) || entry
                    .api_key_env
                    .as_deref()
                    .map(str::trim)
                    .filter(|name| !name.is_empty())
                    .is_some_and(|name| {
                        std::env::var(name).is_ok_and(|value| !value.trim().is_empty())
                    })
            }))
}

/// Validate the configured provider/model/endpoint route without making a
/// network request. This is shared by model inventory, `/model`, and Fleet so
/// none of them can mark a route selectable when `/provider` would reject it.
pub(crate) fn route_is_valid_for_model(
    config: &crate::config::Config,
    identity: &ProviderIdentity,
    model: Option<&str>,
) -> bool {
    let provider = identity.provider;
    if config.verify_provider_identity(identity).is_err() || provider == ProviderKind::Antigravity {
        return false;
    }
    let kind = provider;
    let configured = config.provider_config_for(identity);
    let configured_model = model
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| {
            configured
                .and_then(|entry| entry.model.as_deref())
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        });
    let active_model = (config
        .active_provider_identity()
        .is_ok_and(|active| active == *identity))
    .then(|| config.default_model())
    .filter(|model| !model.trim().is_empty() && !model.eq_ignore_ascii_case("auto"));
    let request = RouteRequest {
        explicit_provider: Some(kind),
        model_selector: configured_model.or(active_model).map(LogicalModelRef::from),
        saved_provider_model: None,
        base_url_override: Some(config.base_url_for_route(identity)),
        limit_overrides: Vec::new(),
    };
    RouteResolver::new()
        .resolve(&request)
        .is_ok_and(|candidate| candidate.validation().ok)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LastProviderCheck {
    Passed,
    /// A 2xx `/models` response proves reachability only — never model readiness.
    ModelsEndpointPassed,
    Failed {
        category: ErrorCategory,
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ResolvedProviderReadiness {
    MissingKey,
    MissingLogin,
    ExternalConsentPendingSelection,
    SavedUnchecked,
    ImportedTokenUnchecked,
    NoAuthUnchecked,
    LocalUnchecked,
    Ready,
    ConnectionCheckedModelUnchecked,
    SavedLastCheckFailed {
        category: ErrorCategory,
        message: String,
    },
    InvalidRoute,
    Legacy,
}

impl ResolvedProviderReadiness {
    pub(crate) fn label(&self) -> Cow<'static, str> {
        match self {
            Self::MissingKey => Cow::Borrowed("missing key"),
            Self::MissingLogin => Cow::Borrowed("missing login"),
            Self::ExternalConsentPendingSelection => {
                Cow::Borrowed("external consent · select to check")
            }
            Self::SavedUnchecked => Cow::Borrowed("key saved · not checked"),
            Self::ImportedTokenUnchecked => Cow::Borrowed("imported token · not checked"),
            Self::NoAuthUnchecked => Cow::Borrowed("no auth · not checked"),
            Self::LocalUnchecked => Cow::Borrowed("local · not checked"),
            Self::Ready => Cow::Borrowed("ready"),
            Self::ConnectionCheckedModelUnchecked => {
                Cow::Borrowed("endpoint reachable · model not checked")
            }
            Self::SavedLastCheckFailed { category, .. } => {
                Cow::Owned(format!("last check failed ({category})"))
            }
            Self::InvalidRoute => Cow::Borrowed("invalid route"),
            Self::Legacy => Cow::Borrowed("legacy"),
        }
    }

    pub(crate) fn detail(&self) -> Option<&str> {
        match self {
            Self::SavedLastCheckFailed { message, .. } => Some(message),
            _ => None,
        }
    }

    pub(crate) fn can_attempt(&self) -> bool {
        matches!(
            self,
            Self::SavedUnchecked
                | Self::NoAuthUnchecked
                | Self::LocalUnchecked
                | Self::ImportedTokenUnchecked
                | Self::Ready
                | Self::SavedLastCheckFailed { .. }
        )
    }

    /// Whether the provider needs an explicit human activation step before it
    /// can be used. Fleet uses this to turn a dormant external-consent route
    /// into a real selection without weakening the global readiness boundary.
    pub(crate) fn requires_explicit_activation(&self) -> bool {
        matches!(self, Self::ExternalConsentPendingSelection)
    }

    /// A short, non-sensitive reason this row cannot be activated. `None`
    /// means the row is either ready or requires explicit activation.
    pub(crate) fn blocked_reason(&self) -> Option<Cow<'static, str>> {
        match self {
            Self::MissingKey => Some(Cow::Borrowed("missing API key")),
            Self::MissingLogin => Some(Cow::Borrowed("missing login")),
            Self::InvalidRoute => Some(Cow::Borrowed("invalid route")),
            Self::Legacy => Some(Cow::Borrowed("legacy route")),
            Self::ConnectionCheckedModelUnchecked => Some(Cow::Borrowed("model not checked")),
            Self::SavedLastCheckFailed { message, .. } => Some(Cow::Owned(message.clone())),
            Self::SavedUnchecked
            | Self::ImportedTokenUnchecked
            | Self::NoAuthUnchecked
            | Self::LocalUnchecked
            | Self::Ready
            | Self::ExternalConsentPendingSelection => None,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct ProviderReadinessSnapshot {
    checks: Vec<(ProviderRouteIdentity, LastProviderCheck)>,
    /// Credentials the provider refused (authentication failures), keyed by
    /// route without the model: a rejected key is rejected for every model.
    /// Unlike `checks`, an entry may carry no credential generation — an
    /// ambient `*_API_KEY`, a secret-store key or a command source cannot be
    /// fingerprinted read-only, and without this the picker kept offering a
    /// refused env key as "key saved · not checked".
    ///
    /// Known limitation: an opaque (generation-less) rejection cannot see a
    /// key rotated outside this process; it lasts until the next success on
    /// that route, or until a saved key makes the generation provable.
    credential_rejections: Vec<(ProviderRouteIdentity, LastProviderCheck)>,
}

fn same_credential(candidate: &ProviderRouteIdentity, identity: &ProviderRouteIdentity) -> bool {
    candidate.identity == identity.identity
        && candidate.endpoint == identity.endpoint
        && candidate.auth_class == identity.auth_class
        && candidate.credential_generation == identity.credential_generation
}

impl ProviderReadinessSnapshot {
    fn last(&self, identity: &ProviderRouteIdentity) -> Option<&LastProviderCheck> {
        // A refused credential outranks any older per-model success with it;
        // a later success on the route clears the rejection.
        if let Some(check) = self
            .credential_rejections
            .iter()
            .rev()
            .find_map(|(candidate, check)| same_credential(candidate, identity).then_some(check))
        {
            return Some(check);
        }
        identity.credential_generation.as_ref()?;
        if let Some(check) = self
            .checks
            .iter()
            .rev()
            .find_map(|(candidate, check)| (candidate == identity).then_some(check))
        {
            return Some(check);
        }
        // The auto model route never records under the literal "auto" —
        // successes and failures are recorded against the concrete model
        // the router actually ran. Without this fallback every auto-mode
        // read would report "not checked" forever, even after hundreds of
        // successful turns on that route.
        if identity.model != "auto" {
            return None;
        }
        self.checks
            .iter()
            .rev()
            .find_map(|(candidate, check)| same_credential(candidate, identity).then_some(check))
    }

    pub(crate) fn record_success(
        &mut self,
        config: &crate::config::Config,
        receipt: &crate::route_receipt::TurnRouteReceipt,
        model: &str,
    ) {
        let identity = receipt.admitted_identity();
        let mut route = route_identity_for_model(config, identity, model);
        route.endpoint = receipt.endpoint_identity().to_string();
        route.credential_generation = Some(receipt.credential_generation().clone());
        self.credential_rejections.retain(|(candidate, _)| {
            candidate.identity != route.identity || candidate.endpoint != route.endpoint
        });
        self.replace(route, LastProviderCheck::Passed);
    }

    /// Records a failed `/models` probe. Unlike [`Self::record_failure`],
    /// this always stores the result so Test Connection can refresh a
    /// stuck `not checked` row.
    pub(crate) fn record_models_probe_failure(
        &mut self,
        config: &crate::config::Config,
        identity: &ProviderIdentity,
        model: &str,
        generation: crate::route_receipt::CredentialGeneration,
        category: ErrorCategory,
        message: &str,
    ) {
        let mut route = route_identity_for_model(config, identity, model);
        route.credential_generation = Some(generation);
        self.replace(
            route,
            LastProviderCheck::Failed {
                category,
                message: sanitize_message(message),
            },
        );
    }

    /// Records a 2xx `/models` probe as connection-checked, never `Ready`.
    pub(crate) fn record_models_probe_success(
        &mut self,
        config: &crate::config::Config,
        identity: &ProviderIdentity,
        model: &str,
        generation: crate::route_receipt::CredentialGeneration,
    ) {
        let mut route = route_identity_for_model(config, identity, model);
        route.credential_generation = Some(generation);
        self.replace(route, LastProviderCheck::ModelsEndpointPassed);
    }

    pub(crate) fn record_failure(
        &mut self,
        config: &crate::config::Config,
        receipt: &crate::route_receipt::TurnRouteReceipt,
        model: &str,
        envelope: &ErrorEnvelope,
    ) {
        let identity = receipt.admitted_identity();
        let mut route = route_identity_for_model(config, identity, model);
        route.endpoint = receipt.endpoint_identity().to_string();
        route.credential_generation = Some(receipt.credential_generation().clone());
        if !provider_owned_failure(envelope) {
            return;
        }
        let check = LastProviderCheck::Failed {
            category: envelope.category,
            message: sanitize_message(&envelope.message),
        };
        if envelope.category == ErrorCategory::Authentication {
            // Record the rejection under the generation the picker will
            // compute. When that projection proves a different credential is
            // configured now, the refused one is already gone.
            let current = config.readonly_health_credential_generation(identity);
            if current
                .as_ref()
                .is_none_or(|generation| generation == receipt.credential_generation())
            {
                let mut rejected = route.clone();
                rejected.model.clear();
                rejected.credential_generation = current;
                self.credential_rejections
                    .retain(|(candidate, _)| !same_credential(candidate, &rejected));
                self.credential_rejections.push((rejected, check.clone()));
            }
        }
        self.replace(route, check);
    }

    #[cfg(test)]
    pub(crate) fn record_failure_message(
        &mut self,
        config: &crate::config::Config,
        identity: &ProviderIdentity,
        model: &str,
        category: ErrorCategory,
        message: &str,
    ) {
        self.replace(
            route_identity_for_model(config, identity, model),
            LastProviderCheck::Failed {
                category,
                message: sanitize_message(message),
            },
        );
    }

    fn replace(&mut self, identity: ProviderRouteIdentity, check: LastProviderCheck) {
        self.checks.retain(|(candidate, _)| candidate != &identity);
        self.checks.push((identity, check));
    }
}

pub(crate) fn resolve_with_identity(
    identity: &ProviderRouteIdentity,
    credentials: CredentialState,
    route_ok: bool,
    checks: &ProviderReadinessSnapshot,
) -> ResolvedProviderReadiness {
    if !route_ok {
        return ResolvedProviderReadiness::InvalidRoute;
    }
    match credentials {
        CredentialState::Legacy => ResolvedProviderReadiness::Legacy,
        CredentialState::MissingKey => ResolvedProviderReadiness::MissingKey,
        CredentialState::MissingLogin => ResolvedProviderReadiness::MissingLogin,
        CredentialState::ExternalConsent => match checks.last(identity) {
            Some(LastProviderCheck::Passed) => ResolvedProviderReadiness::Ready,
            Some(LastProviderCheck::ModelsEndpointPassed) => {
                ResolvedProviderReadiness::ConnectionCheckedModelUnchecked
            }
            Some(LastProviderCheck::Failed { category, message }) => {
                ResolvedProviderReadiness::SavedLastCheckFailed {
                    category: *category,
                    message: message.clone(),
                }
            }
            None => ResolvedProviderReadiness::ExternalConsentPendingSelection,
        },
        CredentialState::Saved
        | CredentialState::ImportedToken
        | CredentialState::NoAuth
        | CredentialState::Local => match checks.last(identity) {
            Some(LastProviderCheck::Passed) => ResolvedProviderReadiness::Ready,
            Some(LastProviderCheck::ModelsEndpointPassed) => {
                ResolvedProviderReadiness::ConnectionCheckedModelUnchecked
            }
            Some(LastProviderCheck::Failed { category, message }) => {
                ResolvedProviderReadiness::SavedLastCheckFailed {
                    category: *category,
                    message: message.clone(),
                }
            }
            None if credentials == CredentialState::NoAuth => {
                ResolvedProviderReadiness::NoAuthUnchecked
            }
            None if credentials == CredentialState::Local => {
                ResolvedProviderReadiness::LocalUnchecked
            }
            None if credentials == CredentialState::ImportedToken => {
                ResolvedProviderReadiness::ImportedTokenUnchecked
            }
            None => ResolvedProviderReadiness::SavedUnchecked,
        },
    }
}

pub(crate) fn resolve_for_model(
    config: &crate::config::Config,
    identity: &ProviderIdentity,
    model: &str,
    checks: &ProviderReadinessSnapshot,
) -> ResolvedProviderReadiness {
    resolve_with_identity(
        &route_identity_for_model(config, identity, model),
        credential_state_for_provider(config, identity),
        route_is_valid_for_model(config, identity, Some(model)),
        checks,
    )
}

fn provider_owned_failure(envelope: &ErrorEnvelope) -> bool {
    matches!(
        envelope.category,
        ErrorCategory::Network
            | ErrorCategory::Authentication
            | ErrorCategory::Authorization
            | ErrorCategory::RateLimit
            | ErrorCategory::Timeout
    )
}

fn sanitize_message(message: &str) -> String {
    crate::utils::truncate_with_ellipsis(message.trim(), 120, "…")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugin_oauth_readiness_requires_login_even_on_a_local_custom_route() {
        let mut config = crate::config::Config {
            provider: Some("plugin-test".into()),
            ..Default::default()
        };
        let entry = config
            .providers
            .get_or_insert_with(Default::default)
            .custom
            .entry("plugin-test".into())
            .or_default();
        entry.kind = Some("openai-compatible".into());
        entry.base_url = Some("http://127.0.0.1:12345/api".into());
        entry.auth_mode = Some("oauth".into());
        entry.oauth = Some(crate::oauth::PluginOAuthConfig {
            issuer: "http://127.0.0.1:12345".into(),
            authorization_endpoint: "http://127.0.0.1:12345/authorize".into(),
            token_endpoint: "http://127.0.0.1:12345/token".into(),
            client_id: "plugin-test".into(),
            scopes: vec!["models:invoke".into()],
            resource: None,
            callback_path: "/oauth/callback".into(),
        });
        let identity = config.active_provider_identity().unwrap();
        // An absent receipt is rejected before any credential file is read.
        assert_eq!(
            auth_class_for_provider(&config, &identity),
            ProviderAuthClass::OAuth
        );
        assert_eq!(
            credential_state_for_provider(&config, &identity),
            CredentialState::MissingLogin
        );
        config.auth_mode = Some("none".into());
        assert_eq!(
            credential_state_for_provider(&config, &identity),
            CredentialState::MissingLogin
        );
    }

    use crate::error_taxonomy::ErrorSeverity;

    fn literal_health_config(kind: ProviderKind) -> crate::config::Config {
        let mut config = crate::config::Config {
            provider: Some(kind.as_str().into()),
            ..Default::default()
        };
        let identity = config.builtin_provider_identity(kind).unwrap();
        config.provider_config_for_mut(&identity).unwrap().api_key =
            Some("health-fixture-key".into());
        config
    }

    fn resolve_test_route(
        config: &crate::config::Config,
        provider: ProviderKind,
        model: &str,
        credentials: CredentialState,
        route_ok: bool,
        checks: &ProviderReadinessSnapshot,
    ) -> ResolvedProviderReadiness {
        resolve_with_identity(
            &route_identity_for_model(config, &(config).test_identity_for_kind(provider), model),
            credentials,
            route_ok,
            checks,
        )
    }

    #[test]
    fn saved_credentials_are_never_ready_without_observed_success() {
        let config = crate::config::Config::default();
        let checks = ProviderReadinessSnapshot::default();
        assert_eq!(
            resolve_test_route(
                &config,
                ProviderKind::Deepseek,
                "deepseek-v4-pro",
                CredentialState::Saved,
                true,
                &checks,
            ),
            ResolvedProviderReadiness::SavedUnchecked
        );
    }

    #[test]
    fn deepseek_cn_compatibility_alias_uses_real_key_readiness() {
        let _lock = crate::test_support::lock_test_env();
        let _key = crate::test_support::EnvVarGuard::remove("DEEPSEEK_API_KEY");
        let missing = crate::config::Config {
            provider: Some("deepseek-cn".to_string()),
            ..Default::default()
        };
        assert_eq!(
            credential_state_for_provider(
                &missing,
                &(missing)
                    .resolve_provider_selection_identity("deepseek-cn")
                    .unwrap()
            ),
            CredentialState::MissingKey
        );

        let configured = crate::config::Config {
            provider: Some("deepseek-cn".to_string()),
            providers: Some(crate::config::ProvidersConfig {
                deepseek_cn: crate::config::ProviderConfig {
                    api_key: Some("deepseek-cn-test-key".to_string()),
                    ..Default::default()
                },
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(
            credential_state_for_provider(
                &configured,
                &(configured)
                    .resolve_provider_selection_identity("deepseek-cn")
                    .unwrap()
            ),
            CredentialState::Saved
        );
    }

    #[test]
    fn custom_readiness_identity_preserves_case_sensitive_route_parts() {
        let custom = std::collections::HashMap::from([
            (
                "CUSTOM".to_string(),
                crate::config::ProviderConfig {
                    kind: Some("openai-compatible".to_string()),
                    base_url: Some("https://example.test/TenantA/v1".to_string()),
                    model: Some("Vendor/ModelA".to_string()),
                    api_key: Some("test-key-a".to_string()),
                    ..Default::default()
                },
            ),
            (
                "custom".to_string(),
                crate::config::ProviderConfig {
                    kind: Some("openai-compatible".to_string()),
                    base_url: Some("https://example.test/tenanta/v1".to_string()),
                    model: Some("vendor/modela".to_string()),
                    api_key: Some("test-key-b".to_string()),
                    ..Default::default()
                },
            ),
        ]);
        let upper = crate::config::Config {
            provider: Some("CUSTOM".to_string()),
            providers: Some(crate::config::ProvidersConfig {
                custom: custom.clone(),
                ..Default::default()
            }),
            ..Default::default()
        };
        let lower = crate::config::Config {
            provider: Some("custom".to_string()),
            providers: Some(crate::config::ProvidersConfig {
                custom,
                ..Default::default()
            }),
            ..Default::default()
        };
        let upper_identity = route_identity_for_model(
            &upper,
            &(upper).test_identity_for_kind(ProviderKind::Custom),
            "Vendor/ModelA",
        );
        let lower_identity = route_identity_for_model(
            &lower,
            &(lower).test_identity_for_kind(ProviderKind::Custom),
            "vendor/modela",
        );

        assert_ne!(upper_identity, lower_identity);
        assert_eq!(upper_identity.identity.key.as_str(), "CUSTOM");
        assert_eq!(upper_identity.endpoint, "https://example.test/TenantA/v1");
        assert_eq!(upper_identity.model, "Vendor/ModelA");

        let mut checks = ProviderReadinessSnapshot::default();
        checks.record_success(
            &upper,
            &crate::route_receipt::TurnRouteReceipt::for_test_fixture(
                &upper,
                ProviderKind::Custom,
                "Vendor/ModelA",
            ),
            "Vendor/ModelA",
        );
        assert_eq!(
            resolve_for_model(
                &lower,
                &(lower).test_identity_for_kind(ProviderKind::Custom),
                "vendor/modela",
                &checks
            ),
            ResolvedProviderReadiness::SavedUnchecked
        );
    }

    #[test]
    fn external_consent_cached_turn_cannot_prove_current_opaque_generation() {
        let config = crate::config::Config::default();
        let mut checks = ProviderReadinessSnapshot::default();
        checks.record_success(
            &config,
            &crate::route_receipt::TurnRouteReceipt::for_test_fixture(
                &config,
                ProviderKind::OpenaiCodex,
                crate::config::DEFAULT_OPENAI_CODEX_MODEL,
            ),
            crate::config::DEFAULT_OPENAI_CODEX_MODEL,
        );
        assert_eq!(
            resolve_test_route(
                &config,
                ProviderKind::OpenaiCodex,
                crate::config::DEFAULT_OPENAI_CODEX_MODEL,
                CredentialState::ExternalConsent,
                true,
                &checks,
            ),
            ResolvedProviderReadiness::ExternalConsentPendingSelection
        );
    }

    #[test]
    fn external_consent_cached_failure_cannot_prove_current_opaque_generation() {
        let config = crate::config::Config::default();
        let mut checks = ProviderReadinessSnapshot::default();
        checks.record_failure_message(
            &config,
            &(config).test_identity_for_kind(ProviderKind::Xai),
            "grok-4.5",
            ErrorCategory::Authentication,
            "consent revoked",
        );
        assert_eq!(
            resolve_test_route(
                &config,
                ProviderKind::Xai,
                "grok-4.5",
                CredentialState::ExternalConsent,
                true,
                &checks
            ),
            ResolvedProviderReadiness::ExternalConsentPendingSelection
        );
    }

    #[test]
    fn external_consent_without_check_stays_pending_selection() {
        let config = crate::config::Config::default();
        let checks = ProviderReadinessSnapshot::default();
        assert_eq!(
            resolve_test_route(
                &config,
                ProviderKind::OpenaiCodex,
                crate::config::DEFAULT_OPENAI_CODEX_MODEL,
                CredentialState::ExternalConsent,
                true,
                &checks,
            ),
            ResolvedProviderReadiness::ExternalConsentPendingSelection
        );
    }

    #[test]
    fn models_probe_success_marks_connection_checked_not_ready() {
        let _lock = crate::test_support::lock_test_env();
        let _cli = crate::test_support::EnvVarGuard::remove("CODEWHALE_CLI_API_KEY");
        let _source = crate::test_support::EnvVarGuard::remove("DEEPSEEK_API_KEY_SOURCE");
        let config = literal_health_config(ProviderKind::Deepseek);
        let mut checks = ProviderReadinessSnapshot::default();
        checks.record_models_probe_success(
            &config,
            &(config).test_identity_for_kind(ProviderKind::Deepseek),
            "deepseek-v4-pro",
            crate::route_receipt::TurnRouteReceipt::for_test_fixture(
                &config,
                ProviderKind::Deepseek,
                "deepseek-v4-pro",
            )
            .credential_generation()
            .clone(),
        );
        assert_eq!(
            resolve_test_route(
                &config,
                ProviderKind::Deepseek,
                "deepseek-v4-pro",
                CredentialState::Saved,
                true,
                &checks,
            ),
            ResolvedProviderReadiness::ConnectionCheckedModelUnchecked
        );
        assert_eq!(
            ResolvedProviderReadiness::ConnectionCheckedModelUnchecked.label(),
            "endpoint reachable · model not checked"
        );
        assert!(!ResolvedProviderReadiness::ConnectionCheckedModelUnchecked.can_attempt());
    }

    #[test]
    fn success_and_provider_failure_replace_session_evidence() {
        let _lock = crate::test_support::lock_test_env();
        let _cli = crate::test_support::EnvVarGuard::remove("CODEWHALE_CLI_API_KEY");
        let _source = crate::test_support::EnvVarGuard::remove("DEEPSEEK_API_KEY_SOURCE");
        let config = literal_health_config(ProviderKind::Zai);
        let mut checks = ProviderReadinessSnapshot::default();
        checks.record_success(
            &config,
            &crate::route_receipt::TurnRouteReceipt::for_test_fixture(
                &config,
                ProviderKind::Zai,
                "glm-5.2",
            ),
            "glm-5.2",
        );
        assert_eq!(
            resolve_test_route(
                &config,
                ProviderKind::Zai,
                "glm-5.2",
                CredentialState::Saved,
                true,
                &checks,
            ),
            ResolvedProviderReadiness::Ready
        );

        checks.record_failure(
            &config,
            &crate::route_receipt::TurnRouteReceipt::for_test_fixture(
                &config,
                ProviderKind::Zai,
                "glm-5.2",
            ),
            "glm-5.2",
            &ErrorEnvelope::new(
                ErrorCategory::Authentication,
                ErrorSeverity::Error,
                false,
                "auth_failed",
                "token rejected",
            ),
        );
        let resolved = resolve_test_route(
            &config,
            ProviderKind::Zai,
            "glm-5.2",
            CredentialState::Saved,
            true,
            &checks,
        );
        assert!(matches!(
            resolved,
            ResolvedProviderReadiness::SavedLastCheckFailed {
                category: ErrorCategory::Authentication,
                ..
            }
        ));
        assert!(resolved.can_attempt());
    }

    #[test]
    fn tool_failures_do_not_poison_provider_health() {
        let _lock = crate::test_support::lock_test_env();
        let _cli = crate::test_support::EnvVarGuard::remove("CODEWHALE_CLI_API_KEY");
        let _source = crate::test_support::EnvVarGuard::remove("DEEPSEEK_API_KEY_SOURCE");
        let config = literal_health_config(ProviderKind::Deepseek);
        let mut checks = ProviderReadinessSnapshot::default();
        checks.record_success(
            &config,
            &crate::route_receipt::TurnRouteReceipt::for_test_fixture(
                &config,
                ProviderKind::Deepseek,
                "deepseek-v4-pro",
            ),
            "deepseek-v4-pro",
        );
        checks.record_failure(
            &config,
            &crate::route_receipt::TurnRouteReceipt::for_test_fixture(
                &config,
                ProviderKind::Deepseek,
                "deepseek-v4-pro",
            ),
            "deepseek-v4-pro",
            &ErrorEnvelope::new(
                ErrorCategory::Tool,
                ErrorSeverity::Error,
                false,
                "tool_failed",
                "shell failed",
            ),
        );
        assert!(matches!(
            resolve_test_route(
                &config,
                ProviderKind::Deepseek,
                "deepseek-v4-pro",
                CredentialState::Saved,
                true,
                &checks,
            ),
            ResolvedProviderReadiness::Ready
        ));
    }

    #[test]
    fn route_and_missing_auth_states_dominate_health() {
        let config = crate::config::Config {
            provider: Some("openai-codex".to_string()),
            ..Default::default()
        };
        let mut checks = ProviderReadinessSnapshot::default();
        checks.record_success(
            &config,
            &crate::route_receipt::TurnRouteReceipt::for_test_fixture(
                &config,
                ProviderKind::OpenaiCodex,
                "gpt-5.5",
            ),
            "gpt-5.5",
        );
        assert_eq!(
            resolve_test_route(
                &config,
                ProviderKind::OpenaiCodex,
                "gpt-5.5",
                CredentialState::MissingLogin,
                true,
                &checks
            ),
            ResolvedProviderReadiness::MissingLogin
        );
        assert_eq!(
            resolve_test_route(
                &config,
                ProviderKind::OpenaiCodex,
                "gpt-5.5",
                CredentialState::Saved,
                false,
                &checks
            ),
            ResolvedProviderReadiness::InvalidRoute
        );
    }

    #[test]
    fn api_key_success_does_not_verify_new_xai_oauth_route() {
        let _lock = crate::test_support::lock_test_env();
        let temp = tempfile::tempdir().expect("oauth fixture root");
        let oauth_path = temp.path().join("grok-auth.json");
        std::fs::write(
            &oauth_path,
            serde_json::to_vec(&serde_json::json!({
                "test-scope": {
                    "key": "expired-access-token",
                    "refresh_token": "saved-refresh-token",
                    "expires_at": "2000-01-01T00:00:00Z",
                    "auth_mode": "oidc"
                }
            }))
            .expect("oauth json"),
        )
        .expect("oauth fixture");
        let _oauth_path = crate::test_support::EnvVarGuard::set("GROK_AUTH_PATH", &oauth_path);

        let api_key_config = crate::config::Config {
            provider: Some("xai".to_string()),
            providers: Some(crate::config::ProvidersConfig {
                xai: crate::config::ProviderConfig {
                    api_key: Some("xai-test-key".to_string()),
                    auth_mode: Some("api_key".to_string()),
                    ..Default::default()
                },
                ..Default::default()
            }),
            ..Default::default()
        };
        let mut checks = ProviderReadinessSnapshot::default();
        let api_key_model = api_key_config.default_model();
        checks.record_success(
            &api_key_config,
            &crate::route_receipt::TurnRouteReceipt::for_test_fixture(
                &api_key_config,
                ProviderKind::Xai,
                &api_key_model,
            ),
            &api_key_model,
        );

        let mut oauth_config = api_key_config;
        oauth_config
            .providers
            .as_mut()
            .expect("providers")
            .xai
            .auth_mode = Some("oauth".to_string());
        assert_eq!(
            credential_state_for_provider(
                &oauth_config,
                &(oauth_config).test_identity_for_kind(ProviderKind::Xai)
            ),
            CredentialState::Saved,
            "fixture must have structurally valid OAuth material"
        );
        let model = oauth_config.default_model();
        assert_eq!(
            resolve_for_model(
                &oauth_config,
                &(oauth_config).test_identity_for_kind(ProviderKind::Xai),
                &model,
                &checks
            ),
            ResolvedProviderReadiness::SavedUnchecked,
            "API-key evidence must not cross the auth-class boundary"
        );
    }

    #[test]
    fn observed_success_is_scoped_to_exact_model_endpoint_and_custom_provider() {
        let deepseek = crate::config::Config {
            ..Default::default()
        }
        .with_legacy_root(Some("deepseek-test-key".to_string()), None);
        let mut checks = ProviderReadinessSnapshot::default();
        checks.record_success(
            &deepseek,
            &crate::route_receipt::TurnRouteReceipt::for_test_fixture(
                &deepseek,
                ProviderKind::Deepseek,
                "deepseek-v4-pro",
            ),
            "deepseek-v4-pro",
        );
        assert_eq!(
            resolve_for_model(
                &deepseek,
                &(deepseek).test_identity_for_kind(ProviderKind::Deepseek),
                "deepseek-v4-flash",
                &checks,
            ),
            ResolvedProviderReadiness::SavedUnchecked,
            "one model entitlement must not verify a sibling model"
        );

        let custom_config = |id: &str, endpoint: &str| crate::config::Config {
            provider: Some(id.to_string()),
            providers: Some(crate::config::ProvidersConfig {
                custom: std::collections::HashMap::from([(
                    id.to_string(),
                    crate::config::ProviderConfig {
                        kind: Some("openai-compatible".to_string()),
                        base_url: Some(endpoint.to_string()),
                        model: Some("private-coder".to_string()),
                        api_key: Some("custom-test-key".to_string()),
                        ..Default::default()
                    },
                )]),
                ..Default::default()
            }),
            ..Default::default()
        };
        let alpha = custom_config("alpha", "https://alpha.example/v1");
        checks.record_success(
            &alpha,
            &crate::route_receipt::TurnRouteReceipt::for_test_fixture(
                &alpha,
                ProviderKind::Custom,
                "private-coder",
            ),
            "private-coder",
        );

        let beta = custom_config("beta", "https://alpha.example/v1");
        assert_eq!(
            resolve_for_model(
                &beta,
                &(beta).test_identity_for_kind(ProviderKind::Custom),
                "private-coder",
                &checks
            ),
            ResolvedProviderReadiness::SavedUnchecked,
            "named custom providers must not share observed health"
        );

        let alpha_moved = custom_config("alpha", "https://other.example/v1");
        assert_eq!(
            resolve_for_model(
                &alpha_moved,
                &(alpha_moved).test_identity_for_kind(ProviderKind::Custom),
                "private-coder",
                &checks,
            ),
            ResolvedProviderReadiness::SavedUnchecked,
            "changing endpoints must invalidate observed health"
        );
    }

    #[test]
    fn auto_model_readiness_follows_the_route_not_the_literal_identity() {
        let config = crate::config::Config {
            ..Default::default()
        }
        .with_legacy_root(Some("test-key".to_string()), None);
        let mut checks = ProviderReadinessSnapshot::default();

        // Before any observed turn, auto honestly reports unchecked.
        assert_eq!(
            resolve_for_model(
                &config,
                &(config).test_identity_for_kind(ProviderKind::Deepseek),
                "auto",
                &checks
            ),
            ResolvedProviderReadiness::SavedUnchecked,
            "auto with no observed history must stay unchecked"
        );

        // A successful concrete-model turn on the route verifies auto too —
        // the router picks the model, so per-model scoping cannot apply.
        checks.record_success(
            &config,
            &crate::route_receipt::TurnRouteReceipt::for_test_fixture(
                &config,
                ProviderKind::Deepseek,
                "deepseek-v4-flash",
            ),
            "deepseek-v4-flash",
        );
        assert_eq!(
            resolve_for_model(
                &config,
                &(config).test_identity_for_kind(ProviderKind::Deepseek),
                "auto",
                &checks
            ),
            ResolvedProviderReadiness::Ready,
            "a passed turn on the route must clear auto's not-checked badge"
        );

        // A later failure on the same route must surface for auto as well.
        checks.record_failure_message(
            &config,
            &(config).test_identity_for_kind(ProviderKind::Deepseek),
            "deepseek-v4-pro",
            crate::error_taxonomy::ErrorCategory::Authentication,
            "401 unauthorized",
        );
        assert!(
            matches!(
                resolve_for_model(
                    &config,
                    &(config).test_identity_for_kind(ProviderKind::Deepseek),
                    "auto",
                    &checks
                ),
                ResolvedProviderReadiness::SavedLastCheckFailed { .. }
            ),
            "the most recent route check must win for auto, including failures"
        );
    }

    #[test]
    fn auto_readiness_does_not_leak_across_providers_or_endpoints() {
        let custom_config = |id: &str, endpoint: &str| crate::config::Config {
            provider: Some(id.to_string()),
            providers: Some(crate::config::ProvidersConfig {
                custom: std::collections::HashMap::from([(
                    id.to_string(),
                    crate::config::ProviderConfig {
                        kind: Some("openai-compatible".to_string()),
                        base_url: Some(endpoint.to_string()),
                        model: Some("private-coder".to_string()),
                        api_key: Some("test-key".to_string()),
                        ..Default::default()
                    },
                )]),
                ..Default::default()
            }),
            ..Default::default()
        };
        let mut checks = ProviderReadinessSnapshot::default();
        let alpha = custom_config("alpha", "https://alpha.example/v1");
        checks.record_success(
            &alpha,
            &crate::route_receipt::TurnRouteReceipt::for_test_fixture(
                &alpha,
                ProviderKind::Custom,
                "private-coder",
            ),
            "private-coder",
        );

        // Same endpoint, different named provider: no leak.
        let beta = custom_config("beta", "https://alpha.example/v1");
        assert_eq!(
            resolve_for_model(
                &beta,
                &(beta).test_identity_for_kind(ProviderKind::Custom),
                "auto",
                &checks
            ),
            ResolvedProviderReadiness::SavedUnchecked,
            "auto fallback is scoped to the route, not the workspace"
        );

        // Same named provider, different endpoint: no leak.
        let alpha_moved = custom_config("alpha", "https://other.example/v1");
        assert_eq!(
            resolve_for_model(
                &alpha_moved,
                &(alpha_moved).test_identity_for_kind(ProviderKind::Custom),
                "auto",
                &checks
            ),
            ResolvedProviderReadiness::SavedUnchecked,
            "changing endpoints must invalidate the auto fallback too"
        );
    }

    #[test]
    fn disabled_external_imports_are_not_probed_by_readiness() {
        let _lock = crate::test_support::lock_test_env();
        let temp = tempfile::tempdir().expect("oauth fixture root");
        let _codewhale_home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", temp.path());
        let kimi_home = temp.path().join("kimi");
        std::fs::create_dir_all(kimi_home.join("credentials")).expect("kimi credentials dir");
        std::fs::write(kimi_home.join("credentials/kimi-code.json"), "{not-json")
            .expect("malformed kimi fixture");
        let _kimi_home = crate::test_support::EnvVarGuard::set(
            "KIMI_CODE_HOME",
            kimi_home.to_str().expect("utf8 path"),
        );
        let grok_path = temp.path().join("grok-auth.json");
        std::fs::write(&grok_path, "{}").expect("empty grok fixture");
        let _grok_path = crate::test_support::EnvVarGuard::set(
            "GROK_AUTH_PATH",
            grok_path.to_str().expect("utf8 path"),
        );

        let config = crate::config::Config {
            providers: Some(crate::config::ProvidersConfig {
                moonshot: crate::config::ProviderConfig {
                    auth_mode: Some("kimi_oauth".to_string()),
                    ..Default::default()
                },
                xai: crate::config::ProviderConfig {
                    auth_mode: Some("oauth".to_string()),
                    ..Default::default()
                },
                ..Default::default()
            }),
            ..Default::default()
        };

        crate::external_credentials::reset_side_effect_trap();
        assert_eq!(
            credential_state_for_provider(
                &config,
                &(config).test_identity_for_kind(ProviderKind::Moonshot)
            ),
            CredentialState::MissingKey,
            "an unusable Kimi import must recover through the supported API-key route"
        );
        assert_eq!(
            credential_state_for_provider(
                &config,
                &(config).test_identity_for_kind(ProviderKind::Xai)
            ),
            CredentialState::MissingLogin
        );
        assert_eq!(
            crate::external_credentials::side_effect_trap_counts(),
            (0, 0),
            "readiness must not inspect external OAuth files without consent"
        );
        assert_eq!(
            std::fs::read_to_string(kimi_home.join("credentials/kimi-code.json"))
                .expect("Kimi fixture unchanged"),
            "{not-json"
        );
        assert_eq!(
            std::fs::read_to_string(&grok_path).expect("Grok fixture unchanged"),
            "{}"
        );

        let api_key_config = crate::config::Config {
            providers: Some(crate::config::ProvidersConfig {
                xai: crate::config::ProviderConfig {
                    api_key: Some("explicit-xai-key".to_string()),
                    ..Default::default()
                },
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(
            credential_state_for_provider(
                &api_key_config,
                &(api_key_config).test_identity_for_kind(ProviderKind::Xai)
            ),
            CredentialState::Saved,
            "an unrelated stale Grok OAuth file must not shadow an explicit xAI API key"
        );
        assert_eq!(
            credential_state_for_provider(
                &crate::config::Config::default(),
                &(crate::config::Config::default()).test_identity_for_kind(ProviderKind::Xai)
            ),
            CredentialState::MissingKey,
            "a Grok file is not active until xAI OAuth is selected in config"
        );
        let stale_root_config = crate::config::Config {
            provider: Some("xai".to_string()),
            ..Default::default()
        }
        .with_legacy_root(Some("legacy-deepseek-root-key".to_string()), None);
        assert_eq!(
            credential_state_for_provider(
                &stale_root_config,
                &(stale_root_config).test_identity_for_kind(ProviderKind::Xai)
            ),
            CredentialState::MissingKey,
            "the legacy root DeepSeek key is not an xAI credential"
        );

        let _cli_source = crate::test_support::EnvVarGuard::set("DEEPSEEK_API_KEY_SOURCE", "cli");
        let _cli_key =
            crate::test_support::EnvVarGuard::set("CODEWHALE_CLI_API_KEY", "explicit-cli-key");
        let cli_config = crate::config::Config {
            provider: Some("xai".to_string()),
            ..Default::default()
        };
        assert_eq!(
            credential_state_for_provider(
                &cli_config,
                &(cli_config).test_identity_for_kind(ProviderKind::Xai)
            ),
            CredentialState::Saved,
            "the source-marked CLI override is valid for the active xAI provider"
        );
    }

    #[test]
    fn custom_provider_env_and_local_no_auth_states_match_runtime() {
        let _lock = crate::test_support::lock_test_env();
        let _custom_key = crate::test_support::EnvVarGuard::set("ACME_CUSTOM_KEY", "custom-secret");
        let remote = crate::config::Config {
            provider: Some("acme".to_string()),
            providers: Some(crate::config::ProvidersConfig {
                custom: std::collections::HashMap::from([(
                    "acme".to_string(),
                    crate::config::ProviderConfig {
                        kind: Some("openai-compatible".to_string()),
                        base_url: Some("https://api.acme.test/v1".to_string()),
                        model: Some("acme-coder".to_string()),
                        api_key_env: Some("ACME_CUSTOM_KEY".to_string()),
                        ..Default::default()
                    },
                )]),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(
            credential_state_for_provider(
                &remote,
                &(remote).test_identity_for_kind(ProviderKind::Custom)
            ),
            CredentialState::Saved
        );

        let local = crate::config::Config {
            provider: Some("local-acme".to_string()),
            providers: Some(crate::config::ProvidersConfig {
                custom: std::collections::HashMap::from([(
                    "local-acme".to_string(),
                    crate::config::ProviderConfig {
                        kind: Some("openai-compatible".to_string()),
                        base_url: Some("http://127.0.0.1:8080/v1".to_string()),
                        model: Some("local-model".to_string()),
                        auth_mode: Some("none".to_string()),
                        ..Default::default()
                    },
                )]),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(
            credential_state_for_provider(
                &local,
                &(local).test_identity_for_kind(ProviderKind::Custom)
            ),
            CredentialState::NoAuth
        );
        assert_eq!(
            resolve_for_model(
                &local,
                &(local).test_identity_for_kind(ProviderKind::Custom),
                "local-model",
                &ProviderReadinessSnapshot::default(),
            ),
            ResolvedProviderReadiness::NoAuthUnchecked
        );
    }

    #[test]
    fn no_auth_has_distinct_readiness_and_cache_identity_from_implicit_local() {
        let local = crate::config::Config {
            provider: Some("vllm".to_string()),
            providers: Some(crate::config::ProvidersConfig {
                vllm: crate::config::ProviderConfig {
                    base_url: Some("http://127.0.0.1:8000/v1".to_string()),
                    model: Some("local-model".to_string()),
                    ..Default::default()
                },
                ..Default::default()
            }),
            ..Default::default()
        };
        let mut no_auth = local.clone();
        no_auth
            .providers
            .as_mut()
            .expect("providers")
            .vllm
            .auth_mode = Some("no-auth".to_string());

        assert_eq!(
            credential_state_for_provider(
                &local,
                &(local).test_identity_for_kind(ProviderKind::Vllm)
            ),
            CredentialState::Local
        );
        assert_eq!(
            credential_state_for_provider(
                &no_auth,
                &(no_auth).test_identity_for_kind(ProviderKind::Vllm)
            ),
            CredentialState::NoAuth
        );
        assert_eq!(
            auth_class_for_provider(&local, &(local).test_identity_for_kind(ProviderKind::Vllm)),
            ProviderAuthClass::Local
        );
        assert_eq!(
            auth_class_for_provider(
                &no_auth,
                &(no_auth).test_identity_for_kind(ProviderKind::Vllm)
            ),
            ProviderAuthClass::NoAuth
        );

        let mut checks = ProviderReadinessSnapshot::default();
        checks.record_success(
            &local,
            &crate::route_receipt::TurnRouteReceipt::for_test_fixture(
                &local,
                ProviderKind::Vllm,
                "local-model",
            ),
            "local-model",
        );
        assert_eq!(
            resolve_for_model(
                &no_auth,
                &(no_auth).test_identity_for_kind(ProviderKind::Vllm),
                "local-model",
                &checks
            ),
            ResolvedProviderReadiness::NoAuthUnchecked,
            "implicit-local success must not verify an explicitly no-auth route"
        );
        assert!(ResolvedProviderReadiness::NoAuthUnchecked.can_attempt());
        assert_eq!(
            ResolvedProviderReadiness::NoAuthUnchecked.label(),
            "no auth · not checked"
        );
    }

    #[test]
    fn ollama_readiness_distinguishes_local_from_cloud_credentials() {
        let _lock = crate::test_support::lock_test_env();
        let temp = tempfile::tempdir().expect("isolated credential home");
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", temp.path());
        let _backend = crate::test_support::EnvVarGuard::set("CODEWHALE_SECRET_BACKEND", "file");
        let _ollama_cloud_key = crate::test_support::EnvVarGuard::remove("OLLAMA_CLOUD_API_KEY");
        let _ollama_key = crate::test_support::EnvVarGuard::remove("OLLAMA_API_KEY");
        let _cli_source = crate::test_support::EnvVarGuard::remove("DEEPSEEK_API_KEY_SOURCE");
        let _cli_key = crate::test_support::EnvVarGuard::remove("CODEWHALE_CLI_API_KEY");

        let local = crate::config::Config {
            provider: Some("ollama".to_string()),
            ..Default::default()
        };
        assert_eq!(
            credential_state_for_provider(
                &local,
                &(local).test_identity_for_kind(ProviderKind::Ollama)
            ),
            CredentialState::Local
        );
        assert_eq!(
            auth_class_for_provider(
                &local,
                &(local).test_identity_for_kind(ProviderKind::Ollama)
            ),
            ProviderAuthClass::Local
        );

        let mut cloud = crate::config::Config {
            provider: Some("ollama".to_string()),
            providers: Some(crate::config::ProvidersConfig {
                ollama: crate::config::ProviderConfig {
                    base_url: Some(codewhale_config::provider::OLLAMA_CLOUD_BASE_URL.to_string()),
                    ..Default::default()
                },
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(
            cloud.active_provider_identity().unwrap().provider,
            ProviderKind::OllamaCloud
        );
        assert_eq!(
            credential_state_for_provider(
                &cloud,
                &(cloud).test_identity_for_kind(ProviderKind::OllamaCloud)
            ),
            CredentialState::MissingKey
        );
        assert_eq!(
            auth_class_for_provider(
                &cloud,
                &(cloud).test_identity_for_kind(ProviderKind::OllamaCloud)
            ),
            ProviderAuthClass::ApiKey
        );

        cloud.providers.as_mut().expect("providers").ollama.api_key =
            Some("ollama-cloud-key".to_string());
        assert_eq!(
            credential_state_for_provider(
                &cloud,
                &(cloud).test_identity_for_kind(ProviderKind::OllamaCloud)
            ),
            CredentialState::Saved
        );
    }

    #[test]
    fn explicit_api_key_mode_on_loopback_requires_a_real_credential() {
        let _lock = crate::test_support::lock_test_env();
        let temp = tempfile::tempdir().expect("isolated credential home");
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", temp.path());
        let _backend = crate::test_support::EnvVarGuard::set("CODEWHALE_SECRET_BACKEND", "file");
        let _vllm_key = crate::test_support::EnvVarGuard::remove("VLLM_API_KEY");
        let _cli_source = crate::test_support::EnvVarGuard::remove("DEEPSEEK_API_KEY_SOURCE");
        let _cli_key = crate::test_support::EnvVarGuard::remove("CODEWHALE_CLI_API_KEY");

        let missing = crate::config::Config {
            provider: Some("vllm".to_string()),
            providers: Some(crate::config::ProvidersConfig {
                vllm: crate::config::ProviderConfig {
                    base_url: Some("http://127.0.0.1:8000/v1".to_string()),
                    model: Some("local-model".to_string()),
                    auth_mode: Some("api_key".to_string()),
                    ..Default::default()
                },
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(
            credential_state_for_provider(
                &missing,
                &(missing).test_identity_for_kind(ProviderKind::Vllm)
            ),
            CredentialState::MissingKey
        );
        assert!(missing.active_route_api_key().is_err());

        let mut configured = missing.clone();
        configured
            .providers
            .as_mut()
            .expect("providers")
            .vllm
            .api_key = Some("protected-local-key".to_string());
        assert_eq!(
            credential_state_for_provider(
                &configured,
                &(configured).test_identity_for_kind(ProviderKind::Vllm)
            ),
            CredentialState::Saved
        );
        assert_eq!(
            configured.active_route_api_key().expect("configured key"),
            "protected-local-key"
        );

        let named_custom = crate::config::Config {
            provider: Some("protected-local".to_string()),
            providers: Some(crate::config::ProvidersConfig {
                custom: std::collections::HashMap::from([(
                    "protected-local".to_string(),
                    crate::config::ProviderConfig {
                        kind: Some("openai-compatible".to_string()),
                        base_url: Some("http://127.0.0.1:9000/v1".to_string()),
                        model: Some("private-model".to_string()),
                        auth_mode: Some("bearer".to_string()),
                        ..Default::default()
                    },
                )]),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(
            credential_state_for_provider(
                &named_custom,
                &(named_custom).test_identity_for_kind(ProviderKind::Custom)
            ),
            CredentialState::MissingKey
        );
        assert!(named_custom.active_route_api_key().is_err());
    }

    #[test]
    fn provider_auth_metadata_is_not_a_runtime_credential() {
        let _lock = crate::test_support::lock_test_env();
        let temp = tempfile::tempdir().expect("isolated credential home");
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", temp.path());
        let _backend = crate::test_support::EnvVarGuard::set("CODEWHALE_SECRET_BACKEND", "file");
        let _openai_key = crate::test_support::EnvVarGuard::remove("OPENAI_API_KEY");
        let _xai_key = crate::test_support::EnvVarGuard::remove("XAI_API_KEY");
        let _cli_source = crate::test_support::EnvVarGuard::remove("DEEPSEEK_API_KEY_SOURCE");
        let _cli_key = crate::test_support::EnvVarGuard::remove("CODEWHALE_CLI_API_KEY");

        let command = crate::config::Config {
            provider: Some("openai".to_string()),
            providers: Some(crate::config::ProvidersConfig {
                openai: crate::config::ProviderConfig {
                    auth: Some(codewhale_config::ProviderAuthSourceToml {
                        source: codewhale_config::AuthSourceKind::Command,
                        command: vec!["secret-tool".to_string(), "lookup".to_string()],
                        timeout_ms: Some(2_000),
                        secret_id: None,
                    }),
                    ..Default::default()
                },
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(
            credential_state_for_provider(
                &command,
                &(command).test_identity_for_kind(ProviderKind::Openai)
            ),
            CredentialState::MissingKey
        );

        let secret = crate::config::Config {
            provider: Some("xai".to_string()),
            providers: Some(crate::config::ProvidersConfig {
                xai: crate::config::ProviderConfig {
                    auth: Some(codewhale_config::ProviderAuthSourceToml {
                        source: codewhale_config::AuthSourceKind::Secret,
                        command: Vec::new(),
                        timeout_ms: None,
                        secret_id: Some("codewhale/xai".to_string()),
                    }),
                    ..Default::default()
                },
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(
            credential_state_for_provider(
                &secret,
                &(secret).test_identity_for_kind(ProviderKind::Xai)
            ),
            CredentialState::MissingKey
        );
    }

    #[test]
    fn oauth_readiness_is_limited_to_official_endpoints() {
        let _lock = crate::test_support::lock_test_env();
        let temp = tempfile::tempdir().expect("isolated oauth home");
        let missing_grok_auth = temp.path().join("missing-grok-auth.json");
        let _grok_auth =
            crate::test_support::EnvVarGuard::set("GROK_AUTH_PATH", &missing_grok_auth);
        let _xai_key = crate::test_support::EnvVarGuard::remove("XAI_API_KEY");
        let _codex_key = crate::test_support::EnvVarGuard::remove("OPENAI_CODEX_ACCESS_TOKEN");
        let _legacy_codex_key = crate::test_support::EnvVarGuard::remove("CODEX_ACCESS_TOKEN");

        let custom_xai = crate::config::Config {
            provider: Some("xai".to_string()),
            providers: Some(crate::config::ProvidersConfig {
                xai: crate::config::ProviderConfig {
                    base_url: Some("https://gateway.example.test/v1".to_string()),
                    auth_mode: Some("oauth".to_string()),
                    ..Default::default()
                },
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(
            auth_class_for_provider(
                &custom_xai,
                &(custom_xai).test_identity_for_kind(ProviderKind::Xai)
            ),
            ProviderAuthClass::ApiKey
        );
        assert_eq!(
            credential_state_for_provider(
                &custom_xai,
                &(custom_xai).test_identity_for_kind(ProviderKind::Xai)
            ),
            CredentialState::MissingKey
        );

        let official_xai = crate::config::Config {
            provider: Some("xai".to_string()),
            providers: Some(crate::config::ProvidersConfig {
                xai: crate::config::ProviderConfig {
                    auth_mode: Some("oauth".to_string()),
                    ..Default::default()
                },
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(
            auth_class_for_provider(
                &official_xai,
                &(official_xai).test_identity_for_kind(ProviderKind::Xai)
            ),
            ProviderAuthClass::OAuth
        );
        assert_eq!(
            credential_state_for_provider(
                &official_xai,
                &(official_xai).test_identity_for_kind(ProviderKind::Xai)
            ),
            CredentialState::MissingLogin
        );

        let custom_codex = crate::config::Config {
            provider: Some("openai-codex".to_string()),
            providers: Some(crate::config::ProvidersConfig {
                openai_codex: crate::config::ProviderConfig {
                    base_url: Some("https://gateway.example.test/v1".to_string()),
                    ..Default::default()
                },
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(
            auth_class_for_provider(
                &custom_codex,
                &(custom_codex).test_identity_for_kind(ProviderKind::OpenaiCodex)
            ),
            ProviderAuthClass::ApiKey
        );
        assert_eq!(
            credential_state_for_provider(
                &custom_codex,
                &(custom_codex).test_identity_for_kind(ProviderKind::OpenaiCodex)
            ),
            CredentialState::MissingKey
        );
    }

    #[test]
    fn xai_custom_endpoint_does_not_count_ambient_official_key() {
        let _lock = crate::test_support::lock_test_env();
        let _source = crate::test_support::EnvVarGuard::remove("DEEPSEEK_API_KEY_SOURCE");
        let _cli_key = crate::test_support::EnvVarGuard::remove("CODEWHALE_CLI_API_KEY");
        let _ambient = crate::test_support::EnvVarGuard::set("XAI_API_KEY", "ambient-xai-key");
        let config = crate::config::Config {
            provider: Some("xai".to_string()),
            providers: Some(crate::config::ProvidersConfig {
                xai: crate::config::ProviderConfig {
                    base_url: Some("https://unrelated-gateway.example.test/v1".to_string()),
                    model: Some("private-grok-model".to_string()),
                    ..Default::default()
                },
                ..Default::default()
            }),
            ..Default::default()
        };

        assert!(!crate::config::has_api_key_for(
            &config,
            &(config).test_identity_for_kind(ProviderKind::Xai)
        ));
        assert_eq!(
            credential_state_for_provider(
                &config,
                &(config).test_identity_for_kind(ProviderKind::Xai)
            ),
            CredentialState::MissingKey
        );
    }

    #[test]
    fn active_deepseek_routes_validate_models_against_effective_custom_base_url() {
        let official = crate::config::Config::default();
        assert!(!route_is_valid_for_model(
            &official,
            &(official).test_identity_for_kind(ProviderKind::Deepseek),
            Some("anthropic/private-model")
        ));

        for provider_name in ["deepseek", "deepseek-cn"] {
            let config = crate::config::Config {
                provider: Some(provider_name.to_string()),
                default_text_model: Some("anthropic/private-model".to_string()),
                ..Default::default()
            }
            .with_legacy_root(
                None,
                Some("https://tenant-gateway.example.test/v1".to_string()),
            );
            let provider = config.active_provider_identity().unwrap().provider;
            assert!(matches!(provider, ProviderKind::Deepseek));
            assert!(route_is_valid_for_model(
                &config,
                &(config).test_identity_for_kind(provider),
                Some("anthropic/private-model")
            ));
        }
    }

    #[test]
    fn cli_forwarded_deepseek_custom_route_validates_prefixed_model() {
        let _lock = crate::test_support::lock_test_env();
        let temp = tempfile::tempdir().expect("isolated config home");
        let config_path = temp.path().join("config.toml");
        std::fs::write(
            &config_path,
            r#"api_key = "saved-file-key"
base_url = "https://api.deepseek.com/v1"
default_text_model = "deepseek-chat"
"#,
        )
        .expect("write config");
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", temp.path());
        let _provider = crate::test_support::EnvVarGuard::set("CODEWHALE_PROVIDER", "deepseek");
        let _legacy_provider = crate::test_support::EnvVarGuard::remove("DEEPSEEK_PROVIDER");
        let _base = crate::test_support::EnvVarGuard::set(
            "CODEWHALE_BASE_URL",
            "https://tenant-gateway.example.test/v1",
        );
        let _legacy_base = crate::test_support::EnvVarGuard::remove("DEEPSEEK_BASE_URL");
        let _model =
            crate::test_support::EnvVarGuard::set("CODEWHALE_MODEL", "anthropic/private-model");
        let _source = crate::test_support::EnvVarGuard::set("DEEPSEEK_API_KEY_SOURCE", "cli");
        let _cli_key =
            crate::test_support::EnvVarGuard::set("CODEWHALE_CLI_API_KEY", "explicit-cli-key");

        let config = crate::config::Config::load(Some(config_path), None).expect("load config");
        assert_eq!(config.default_model(), "anthropic/private-model");
        assert_eq!(
            config.active_route_api_key().expect("explicit CLI key"),
            "explicit-cli-key"
        );
        assert!(route_is_valid_for_model(
            &config,
            &(config).test_identity_for_kind(ProviderKind::Deepseek),
            Some("anthropic/private-model")
        ));
    }

    #[test]
    fn builtin_loopback_local_and_api_key_routes_have_distinct_cache_identity() {
        let _lock = crate::test_support::lock_test_env();
        let _openai_key = crate::test_support::EnvVarGuard::remove("OPENAI_API_KEY");
        let _source = crate::test_support::EnvVarGuard::remove("DEEPSEEK_API_KEY_SOURCE");
        let _cli_key = crate::test_support::EnvVarGuard::remove("CODEWHALE_CLI_API_KEY");
        let local = crate::config::Config {
            provider: Some("openai".to_string()),
            providers: Some(crate::config::ProvidersConfig {
                openai: crate::config::ProviderConfig {
                    base_url: Some("http://127.0.0.1:8080/v1".to_string()),
                    model: Some("local-model".to_string()),
                    ..Default::default()
                },
                ..Default::default()
            }),
            ..Default::default()
        };
        let mut protected = local.clone();
        let protected_route = &mut protected.providers.as_mut().expect("providers").openai;
        protected_route.auth_mode = Some("api_key".to_string());
        protected_route.api_key = Some("protected-local-key".to_string());

        assert_eq!(
            credential_state_for_provider(
                &local,
                &(local).test_identity_for_kind(ProviderKind::Openai)
            ),
            CredentialState::Local
        );
        assert_eq!(
            auth_class_for_provider(
                &local,
                &(local).test_identity_for_kind(ProviderKind::Openai)
            ),
            ProviderAuthClass::Local
        );
        assert_eq!(
            credential_state_for_provider(
                &protected,
                &(protected).test_identity_for_kind(ProviderKind::Openai)
            ),
            CredentialState::Saved
        );
        assert_eq!(
            auth_class_for_provider(
                &protected,
                &(protected).test_identity_for_kind(ProviderKind::Openai)
            ),
            ProviderAuthClass::ApiKey
        );
        assert_ne!(
            route_identity_for_model(
                &local,
                &(local).test_identity_for_kind(ProviderKind::Openai),
                "local-model"
            ),
            route_identity_for_model(
                &protected,
                &(protected).test_identity_for_kind(ProviderKind::Openai),
                "local-model"
            )
        );

        let mut checks = ProviderReadinessSnapshot::default();
        checks.record_success(
            &local,
            &crate::route_receipt::TurnRouteReceipt::for_test_fixture(
                &local,
                ProviderKind::Openai,
                "local-model",
            ),
            "local-model",
        );
        assert_eq!(
            resolve_for_model(
                &protected,
                &(protected).test_identity_for_kind(ProviderKind::Openai),
                "local-model",
                &checks
            ),
            ResolvedProviderReadiness::SavedUnchecked
        );

        let deepseek_cn_local = crate::config::Config {
            provider: Some("deepseek-cn".to_string()),
            default_text_model: Some("local-cn-model".to_string()),
            ..Default::default()
        }
        .with_legacy_root(None, Some("http://127.0.0.1:9090/v1".to_string()));
        assert_eq!(
            credential_state_for_provider(
                &deepseek_cn_local,
                &(deepseek_cn_local)
                    .resolve_provider_selection_identity("deepseek-cn")
                    .unwrap()
            ),
            CredentialState::Local
        );
    }
    #[test]
    fn captured_key_rotation_cannot_reuse_ready_or_failure_evidence() {
        let _env = crate::test_support::lock_test_env();
        let _cli = crate::test_support::EnvVarGuard::remove(codewhale_config::CLI_API_KEY_ENV);
        let mut config = crate::config::Config::from_saved_document(
            r#"provider = "openai"
            [providers.openai]
            api_key = "captured-key"
            model = "gpt-5.5"
        "#,
            None,
        )
        .unwrap();
        let identity = config.active_provider_identity().unwrap();
        let endpoint = config.base_url_for_route(&identity);
        let receipt = crate::route_receipt::TurnRouteReceipt::from_admitted(
            &identity,
            "gpt-5.5",
            &endpoint,
            "captured-key",
        );
        let mut checks = ProviderReadinessSnapshot::default();
        checks.record_success(&config, &receipt, "gpt-5.5");
        let route = route_identity_for_model(&config, &identity, "gpt-5.5");
        assert!(matches!(
            checks.last(&route),
            Some(LastProviderCheck::Passed)
        ));
        config
            .set_provider_api_key_override(&identity, Some("replacement-key".into()))
            .unwrap();
        let replaced = route_identity_for_model(&config, &identity, "gpt-5.5");
        assert_ne!(route.credential_generation, replaced.credential_generation);
        assert!(checks.last(&replaced).is_none());
        checks.record_failure(
            &config,
            &receipt,
            "gpt-5.5",
            &ErrorEnvelope::new(
                ErrorCategory::Authentication,
                ErrorSeverity::Error,
                false,
                "auth_failed",
                "captured route failed",
            ),
        );
        assert!(
            checks.last(&replaced).is_none(),
            "a late failure uses its dispatched key generation"
        );
        assert!(matches!(
            checks.last(&route),
            Some(LastProviderCheck::Failed { .. })
        ));
    }

    #[test]
    fn opaque_command_credentials_do_not_execute_or_inherit_cached_readiness() {
        let _env = crate::test_support::lock_test_env();
        let _cli = crate::test_support::EnvVarGuard::remove(codewhale_config::CLI_API_KEY_ENV);
        let mut config = crate::config::Config::from_saved_document(
            r#"provider = "openai"
            [providers.openai]
            api_key = "captured-key"
            model = "gpt-5.5"
        "#,
            None,
        )
        .unwrap();
        let identity = config.active_provider_identity().unwrap();
        let endpoint = config.base_url_for_route(&identity);
        let receipt = crate::route_receipt::TurnRouteReceipt::from_admitted(
            &identity,
            "gpt-5.5",
            &endpoint,
            "captured-key",
        );
        let mut checks = ProviderReadinessSnapshot::default();
        checks.record_success(&config, &receipt, "gpt-5.5");
        config.provider_config_for_mut(&identity).unwrap().auth =
            Some(codewhale_config::ProviderAuthSourceToml {
                source: codewhale_config::AuthSourceKind::Command,
                command: vec!["this-command-must-never-run".into()],
                timeout_ms: None,
                secret_id: None,
            });
        assert!(
            config
                .readonly_health_credential_generation(&identity)
                .is_none()
        );
        let opaque = route_identity_for_model(&config, &identity, "gpt-5.5");
        assert!(checks.last(&opaque).is_none());
        assert!(!format!("{opaque:?}").contains("captured-key"));
        assert!(!format!("{receipt:?}").contains("captured-key"));
    }

    #[test]
    fn bound_environment_rotation_and_keyless_endpoint_scope_are_exact() {
        let _env = crate::test_support::lock_test_env();
        let _cli = crate::test_support::EnvVarGuard::remove(codewhale_config::CLI_API_KEY_ENV);
        let config = crate::config::Config::from_saved_document(
            r#"provider = "CustomA"
            [providers.CustomA]
            kind = "openai-compatible"
            base_url = "http://127.0.0.1:18180/v1"
            model = "private-model"
            api_key_env = "CODEWHALE_HEALTH_FIXTURE_KEY"
        "#,
            None,
        )
        .unwrap();
        let identity = config.active_provider_identity().unwrap();
        let first =
            crate::test_support::EnvVarGuard::set("CODEWHALE_HEALTH_FIXTURE_KEY", "first-key");
        let generation = config
            .readonly_health_credential_generation(&identity)
            .unwrap();
        drop(first);
        let _second =
            crate::test_support::EnvVarGuard::set("CODEWHALE_HEALTH_FIXTURE_KEY", "second-key");
        assert_ne!(
            config
                .readonly_health_credential_generation(&identity)
                .unwrap(),
            generation
        );
        let local = crate::config::Config::from_saved_document(
            r#"provider = "CustomA"
            [providers.CustomA]
            kind = "openai-compatible"
            base_url = "http://127.0.0.1:18180/v1"
            auth_mode = "none"
        "#,
            None,
        )
        .unwrap();
        let id = local.active_provider_identity().unwrap();
        assert!(local.readonly_health_credential_generation(&id).is_some());
        let mut changed = local.clone();
        changed
            .set_provider_base_url_override(&id, Some("http://127.0.0.1:19191/v1".into()))
            .unwrap();
        assert_ne!(
            local.readonly_health_credential_generation(&id),
            changed.readonly_health_credential_generation(&id)
        );
    }
}

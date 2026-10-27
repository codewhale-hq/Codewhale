//! The one place provider credential precedence is decided.
//!
//! Ported from pi-mono `packages/ai/src/auth/resolve.ts` (MIT, Copyright (c)
//! 2025 Mario Zechner; full notice in `codewhale_runtime::credentials`). The idea taken is
//! pi's: a single resolver, one precedence rule stated in a doc comment beside
//! it, and a result that names the place it resolved from. The walk itself is
//! CodeWhale's — it is the former body of `has_api_key_for`, moved here
//! unchanged in order so no existing decision changes, with a
//! [`CredentialSource`] attached to each outcome.
//!
//! # Precedence rule
//!
//! **An endpoint-bound exact session key on an authenticated eligible route
//! outranks durable and ambient sources. Required account ownership forbids
//! every fallback.**
//!
//! CodeWhale's order below is that rule instantiated over the stores it
//! actually has. Reading top to bottom:
//!
//! 1. Required Codewhale account model ownership, with no fallback.
//! 2. A process-only session key for the exact provider identity and endpoint,
//!    subject to the durable secret store's route eligibility rules.
//! 3. `auth_mode = "none"` — the route sends no credential at all.
//! 4. An explicit `--api-key` on the active, non-OAuth provider.
//! 5. `[providers.<name>] api_key_env` — a credential the route *names*.
//! 6. An ambient provider environment variable (official endpoints only).
//! 7. Provider-owned login state: an explicitly consented supported external
//!    CLI credential file (Codex or DeepSeek Harness) or CodeWhale's own xAI
//!    OAuth storage.
//! 8. A keyless self-hosted / loopback route.
//! 9. `[providers.<name>] api_key` in the config file.
//! 10. CodeWhale's durable secret store.
//! 11. The root `api_key` compatibility slot.
//! 12. The user-global `~/.codewhale/config.toml`.
//! 13. Codewhale account model access when no owner is required.
//!
//! Two departures from pi are deliberate and load-bearing here:
//!
//! * Ambient env outranks the secret store for a *named* binding (step 5) and
//!   for official-endpoint provider variables (step 6). That is CodeWhale's
//!   existing, documented behavior and users depend on it; changing it is not
//!   in this lane's scope. It is stated here so it is at least *visible*.
//! * External CLI credential files are only ever consulted through
//!   [`Config::external_credential_read_grant`], which enforces the read-only
//!   consent model (exact path, explicit consent, never refreshed, never
//!   rewritten). This resolver adds no new way to reach them, and #5772 tightens
//!   the two ends of that model:
//!   - **Nothing happens before consent.** With no persisted consent record
//!     for a provider, this resolver resolves no candidate path, performs no
//!     filesystem access, and names no location. Deriving a candidate from
//!     `HOME` just to say "absent" is itself an unconsented disclosure of where
//!     another CLI keeps credentials.
//!   - **A consent record is not a credential.** Consent proves the user
//!     authorized reading one exact file; it does not prove that file still
//!     holds a usable token. Once consent exists, the consented file is read
//!     through the secure adapter — the read the user actually authorized — so
//!     a missing, malformed, or expired external credential resolves as missing
//!     rather than masquerading as a stored one.
//!
//! # Redaction
//!
//! This module never returns, logs, or renders secret material. It returns
//! only a [`CredentialSource`] label. Every probe that needs a value calls an
//! existing helper and discards the value with `.is_some()`.

use super::*;
use crate::credentials::{
    AuthContext, CredentialProbe, CredentialResolution, CredentialSource,
    context::ProcessAuthContext,
};

/// Resolve which place holds a credential for `provider`, using the real
/// process environment.
pub fn resolve_credential_source(
    config: &Config,
    identity: &ProviderIdentity,
) -> CredentialResolution {
    resolve_credential_source_with(config, identity, &ProcessAuthContext)
}

/// Resolve with an injected [`AuthContext`].
///
/// Only the ambient reads this function performs *itself* go through `ctx`.
/// The provider-specific helpers it delegates to (secret store, external
/// grants, xAI OAuth) still read the real environment and filesystem; making
/// those injectable means threading a context through config.rs and is not in
/// this lane.
pub(crate) fn resolve_credential_source_with(
    config: &Config,
    identity: &ProviderIdentity,
    ctx: &dyn AuthContext,
) -> CredentialResolution {
    if let Err(error) = config.verify_provider_identity(identity) {
        return CredentialResolution::missing(vec![CredentialProbe::with_fix(
            error,
            "repair the exact provider kind/id pair",
        )]);
    }
    if config.required_account_model_owner.is_some() {
        return if config.account_model_api_key(identity).is_some() {
            CredentialResolution::found(CredentialSource::AccountSession)
        } else {
            CredentialResolution::missing(vec![CredentialProbe::with_fix(
                "required Codewhale account model access",
                "reconnect model access for the signed-in Codewhale account",
            )])
        };
    }
    if config.session_provider_api_key(identity).is_some() {
        return CredentialResolution::found(CredentialSource::SessionKey);
    }
    if let Some(source) = config.session_policy_without_key(identity) {
        return CredentialResolution { source };
    }
    let provider = identity.provider;
    let mut probed: Vec<CredentialProbe> = Vec::new();

    let auth_mode = config.auth_mode_for_provider(identity);
    if auth_mode_disables_api_key(auth_mode.as_deref()) {
        return CredentialResolution::found(CredentialSource::AuthModeNone);
    }

    // The public ChatGPT API accepts only Codewhale's own issued registration.
    // Environment tokens and consented Codex files belong to different clients.
    if provider == ProviderKind::Anthropic && auth_mode.as_deref() == Some("oauth") {
        if !config.provider_uses_custom_endpoint(identity)
            && let Some(sign_in) =
                crate::oauth::usable_sign_in(crate::oauth::OAuthProvider::Claude, config)
        {
            return CredentialResolution::found(CredentialSource::OAuth {
                flow: "Claude".to_string(),
                account: sign_in.account_label,
            });
        }
        return CredentialResolution::missing(vec![CredentialProbe::with_fix(
            "Codewhale-owned Claude sign-in",
            "codewhale auth claude",
        )]);
    }
    if provider == ProviderKind::OpenaiCodex && !config.provider_uses_custom_endpoint(identity) {
        if let Some(sign_in) =
            crate::oauth::usable_sign_in(crate::oauth::OAuthProvider::Chatgpt, config)
        {
            return CredentialResolution::found(CredentialSource::OAuth {
                flow: "ChatGPT".to_string(),
                account: sign_in.account_label,
            });
        }
        probed.push(CredentialProbe::with_fix(
            "Codewhale-owned ChatGPT sign-in",
            "codewhale auth chatgpt",
        ));
        return CredentialResolution::missing(probed);
    }

    if config
        .active_provider_identity()
        .is_ok_and(|active| active == *identity)
        && !provider_uses_oauth_credentials(config, identity)
        && explicit_cli_api_key_override().is_some()
    {
        return CredentialResolution::found(CredentialSource::CliOverride);
    }

    if let Some(var) = bound_provider_api_key_env_name(config, identity) {
        if provider_config_env_api_key(config, identity).is_some() {
            return CredentialResolution::found(CredentialSource::ProviderConfigEnv { var });
        }
        probed.push(CredentialProbe::with_fix(
            format!("env {var} (bound by api_key_env)"),
            format!("export {var}=<key>"),
        ));
    }

    let skip_secret_store = config.should_skip_secret_store_for_provider(identity);
    if !skip_secret_store {
        if let Some(var) = provider
            .provider()
            .env_vars()
            .iter()
            .find(|var| ctx.env(var).is_some())
        {
            return CredentialResolution::found(CredentialSource::AmbientEnv {
                var: (*var).to_string(),
            });
        }
        if let Some(var) = provider.provider().env_vars().first() {
            probed.push(CredentialProbe::with_fix(
                format!("env {}", provider.provider().env_vars().join(" / ")),
                format!("export {var}=<key>"),
            ));
        }
    }

    if provider == ProviderKind::Moonshot && provider_uses_oauth_credentials(config, identity) {
        // Kimi CLI credentials are never imported; the route needs its own key.
        probed.push(CredentialProbe::with_fix(
            "Kimi CLI credentials (never imported)",
            "codewhale auth set --provider moonshot",
        ));
        return CredentialResolution::missing(probed);
    }
    if provider == ProviderKind::Xai
        && !config.provider_uses_custom_endpoint(identity)
        && let Some(sign_in) =
            crate::oauth::usable_sign_in(crate::oauth::OAuthProvider::Xai, config)
    {
        // xAI supports both API keys and OAuth. A Grok-compatible token file is
        // sufficient, but its absence must fall through to the ordinary API-key
        // checks below instead of masking a configured key.
        return CredentialResolution::found(CredentialSource::OAuth {
            flow: "xAI".to_string(),
            account: sign_in.account_label,
        });
    }
    if matches!(
        provider,
        ProviderKind::Deepseek | ProviderKind::DeepseekAnthropic
    ) && identity.key.as_str() != codewhale_config::descriptors::LEGACY_DEEPSEEK_CN.id
        && !config.provider_uses_custom_endpoint(identity)
    {
        match resolve_external_grant(
            config,
            identity,
            codewhale_config::ExternalCredentialSource::DshCli,
            "DeepSeek Harness",
            "codewhale auth external-consent --provider deepseek --mode read-only",
            |grant| {
                crate::dsh_credentials::deepseek_api_key_from_grant(grant)
                    .ok()
                    .flatten()
                    .is_some()
            },
        ) {
            Ok(source) => return CredentialResolution::found(source),
            Err(probe) => probed.push(probe),
        }
    }

    if !auth_mode_requires_api_key(auth_mode.as_deref())
        && (provider_route_is_keyless_self_hosted(provider, &config.base_url_for_route(identity))
            || (config
                .active_provider_identity()
                .is_ok_and(|active| active == *identity)
                && base_url_uses_local_host(&config.active_route_base_url())))
    {
        return CredentialResolution::found(CredentialSource::KeylessRoute {
            base_url: config.base_url_for_route(identity),
        });
    }

    if config.config_credentials_are_bound_to_provider_endpoint(identity) {
        if config
            .provider_route_string_with_deepseek_fallback(identity, |entry| entry.api_key.clone())
            .is_some_and(|key| {
                classify_config_api_key_value(&key) == ConfigApiKeyValueKind::Literal
            })
        {
            return CredentialResolution::found(CredentialSource::ProviderConfigApiKey {
                table: provider_config_table_name(identity)
                    .unwrap_or_else(|_| format!("providers.{}", identity.key.as_str())),
            });
        }
        if let Ok(table) = provider_config_table_name(identity) {
            probed.push(CredentialProbe::with_fix(
                format!("[{table}] api_key"),
                format!("add api_key to [{table}] in ~/.codewhale/config.toml"),
            ));
        }
    }
    // Probe the active provider, plus any provider whose persisted
    // `[providers.<name>]` table carries the marker the secret-store save
    // path itself writes (an api-key auth mode with no config literal). A
    // configured-but-inactive provider must not render as unconfigured just
    // because the operator switched providers after saving its key (#5033).
    // Shared-slot families (one account, several provider variants — e.g.
    // Model Studio Token/Coding Plan × OpenAI/Anthropic dialects) honor the
    // marker written by ANY sibling variant, since the save path stores one
    // key under the family's canonical slot. The probe stays bounded to
    // explicitly configured providers, and the non-active case is strictly
    // read-only so rendering the catalog never migrates a legacy store or
    // opens a write-capable backend.
    if !skip_secret_store {
        let slot = provider_secret_store_slot(provider).to_string();
        if config
            .active_provider_identity()
            .is_ok_and(|active| active == *identity)
        {
            if provider_secret_store_api_key(config, identity).is_some() {
                return CredentialResolution::found(CredentialSource::SecretStore { slot });
            }
            probed.push(secret_store_probe(&slot, identity));
        } else if secret_slot_save_marker_on_shared_slot(config, identity) {
            if provider_secret_store_api_key_with_mode(config, identity, true).is_some() {
                return CredentialResolution::found(CredentialSource::SecretStore { slot });
            }
            probed.push(secret_store_probe(&slot, identity));
        } else {
            // #5033's marker gate: without a `[providers.<name>]` api-key
            // auth-mode marker the store is not read at all for an inactive
            // provider. Say so, because the row is otherwise indistinguishable
            // from a genuinely empty slot — and the request path *would* read
            // it once this provider became active.
            probed.push(CredentialProbe::with_fix(
                format!(
                    "secret store \"{slot}\" (not read: inactive provider, no api-key marker)"
                ),
                format!(
                    "codewhale auth set --provider {} writes the marker that makes this slot readable while inactive",
                    identity.key.as_str()
                ),
            ));
        }
    }

    // Last resort: the user-global config file. A key saved there must not
    // disappear just because this process loaded a workspace config.
    if user_global_config_api_key(identity).is_some() {
        return CredentialResolution::found(CredentialSource::UserGlobalConfig);
    }
    probed.push(CredentialProbe::with_fix(
        "~/.codewhale/config.toml",
        format!("codewhale auth set --provider {}", identity.key.as_str()),
    ));

    if config.account_model_api_key(identity).is_some() {
        return CredentialResolution::found(CredentialSource::AccountSession);
    }

    CredentialResolution::missing(probed)
}

fn secret_store_probe(slot: &str, identity: &ProviderIdentity) -> CredentialProbe {
    CredentialProbe::with_fix(
        format!("secret store \"{slot}\""),
        format!("codewhale auth set --provider {}", identity.key.as_str()),
    )
}

/// Resolve one external CLI credential owner for `provider` (#5772).
///
/// The order here is the whole invariant, and each step is gated on the one
/// before it:
///
/// 1. **No consent record** — nothing is resolved, stat'ed, read, or named.
///    The probe offers only the explicit consent command, because deriving a
///    candidate path from `HOME` in order to report it would already disclose
///    where another CLI keeps credentials.
/// 2. **Consent record, provider not active** — the grant is refused by
///    [`Config::external_credential_read_grant`], so the record is reported as
///    dormant. Still no filesystem access.
/// 3. **Consent record, provider active** — the exact consented file is read
///    through the secure adapter and `validate` decides whether it holds a
///    usable credential. Structural consent alone never resolves as found.
fn resolve_external_grant(
    config: &Config,
    identity: &ProviderIdentity,
    source: codewhale_config::ExternalCredentialSource,
    cli: &str,
    consent_command: &str,
    validate: impl FnOnce(&codewhale_config::ExternalCredentialReadGrant) -> bool,
) -> Result<CredentialSource, CredentialProbe> {
    let Some(consent) = config
        .provider_config_for(identity)
        .and_then(|entry| entry.external_credentials.as_ref())
    else {
        return Err(CredentialProbe::with_fix(
            format!("{cli} credentials (no read-only consent recorded)"),
            consent_command.to_string(),
        ));
    };
    // The pinned path comes from the consent record the user confirmed, never
    // from an ambient candidate, so no resolver runs here either.
    let Ok(grant) = config.external_credential_read_grant(identity, source, &consent.path) else {
        return Err(CredentialProbe::with_fix(
            format!("{cli} credentials (consent dormant until this provider is selected)"),
            format!("codewhale config set provider {}", identity.key.as_str()),
        ));
    };
    if validate(&grant) {
        return Ok(CredentialSource::ExternalGrant {
            cli: cli.to_string(),
            path: consent.path.display().to_string(),
        });
    }
    // Consented, read, and unusable: missing, malformed, or expired. Read-only
    // consent never refreshes another CLI's file, so the fix is to renew it
    // there — not to re-consent here.
    Err(CredentialProbe::with_fix(
        format!("{cli} credentials (consented, but no usable credential in that file)"),
        format!(
            "log in again with {cli}, or run codewhale auth set --provider {}",
            identity.key.as_str()
        ),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::test_support::EnvVarGuard;

    use crate::test_support::lock_test_env;

    use crate::credentials::context::MapAuthContext;

    fn deepseek_config() -> Config {
        Config {
            provider: Some("deepseek".to_string()),
            ..Config::default()
        }
    }

    /// The precedence rule has to be enforced somewhere a test can see it.
    #[test]
    fn a_named_env_binding_resolves_and_names_itself() {
        let _lock = lock_test_env();
        let _key = EnvVarGuard::set("CW_TEST_BOUND_KEY", "bound-value");
        let config = Config {
            provider: Some("openrouter".to_string()),
            providers: Some(
                toml::from_str("[openrouter]\napi_key_env = \"CW_TEST_BOUND_KEY\"\n")
                    .expect("provider table"),
            ),
            ..Config::default()
        };
        let resolution = resolve_credential_source(
            &config,
            &(config).test_identity_for_kind(ProviderKind::Openrouter),
        );
        assert_eq!(
            resolution.source,
            CredentialSource::ProviderConfigEnv {
                var: "CW_TEST_BOUND_KEY".to_string()
            },
            "a route that names its variable must resolve from it and say so"
        );
        assert_eq!(resolution.source.label(), "api_key_env CW_TEST_BOUND_KEY");
    }

    /// An ambient export must name the exact variable that won, not just
    /// "configured" — this is pi's `source: "ANTHROPIC_API_KEY"`.
    #[test]
    fn ambient_env_names_the_variable_that_won() {
        let _lock = lock_test_env();
        let ctx = MapAuthContext::new().with_env("OPENROUTER_API_KEY", "value");
        let config = Config::default();
        let resolution = resolve_credential_source_with(
            &config,
            &(config).test_identity_for_kind(ProviderKind::Openrouter),
            &ctx,
        );
        assert_eq!(
            resolution.source,
            CredentialSource::AmbientEnv {
                var: "OPENROUTER_API_KEY".to_string()
            }
        );
        assert_eq!(resolution.source.label(), "OPENROUTER_API_KEY");
    }

    /// The regression this whole lane exists for: a provider with no
    /// credential anywhere used to report a bare boolean. It must now name
    /// every place that was probed, in precedence order, and offer a fix.
    #[test]
    fn a_missing_credential_names_every_place_that_was_checked() {
        let _lock = lock_test_env();
        let ctx = MapAuthContext::new();
        let config = Config::default();
        let resolution = resolve_credential_source_with(
            &config,
            &(config).test_identity_for_kind(ProviderKind::Openrouter),
            &ctx,
        );
        assert!(!resolution.is_present());

        let checked = resolution.checked_places();
        assert!(
            checked.contains("OPENROUTER_API_KEY"),
            "the ambient variable must be named: {checked}"
        );
        assert!(
            checked.contains("secret store \"openrouter\""),
            "the durable slot must be named: {checked}"
        );
        assert!(
            checked.contains("~/.codewhale/config.toml"),
            "the user-global config must be named: {checked}"
        );
        assert_eq!(
            resolution.first_fix(),
            Some("export OPENROUTER_API_KEY=<key>"),
            "the first probed place must carry the command that fixes it"
        );
    }

    /// #5033's marker gate is a real asymmetry between what the picker
    /// reports and what the request path would find: for a provider that is
    /// not active and whose config table carries no api-key marker, the
    /// durable slot is *not read at all*. That is defensible, but it must be
    /// visible — a user staring at "missing key" has to be told the slot was
    /// skipped rather than found empty.
    #[test]
    fn an_unread_secret_slot_says_it_was_not_read_and_why() {
        let _lock = lock_test_env();
        let ctx = MapAuthContext::new();
        let config = deepseek_config();
        let resolution = resolve_credential_source_with(
            &config,
            &(config).test_identity_for_kind(ProviderKind::Openrouter),
            &ctx,
        );

        let checked = resolution.checked_places();
        assert!(
            checked.contains("(not read: inactive provider, no api-key marker)"),
            "an unread slot must not look like an empty one: {checked}"
        );
    }

    /// `auth_mode = "none"` is a resolution, not an absence.
    #[test]
    fn no_auth_routes_resolve_to_the_auth_mode_itself() {
        let _lock = lock_test_env();
        let config = Config {
            providers: Some(
                toml::from_str("[openrouter]\nauth_mode = \"none\"\n").expect("provider table"),
            ),
            ..Config::default()
        };
        let resolution = resolve_credential_source(
            &config,
            &(config).test_identity_for_kind(ProviderKind::Openrouter),
        );
        assert_eq!(resolution.source, CredentialSource::AuthModeNone);
        assert!(resolution.is_present());
        assert!(resolution.checked_places().is_empty());
    }
}

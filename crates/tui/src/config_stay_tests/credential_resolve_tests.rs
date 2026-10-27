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

use codewhale_config::ProviderKind;
use codewhale_runtime::config::*;

use crate::provider_readiness::has_api_key_for;

use crate::test_support::EnvVarGuard;

use crate::test_support::lock_test_env;

/// The resolver is the sole authority; `has_api_key_for` must agree with
/// it for every provider, or two surfaces can disagree again.
#[test]
fn has_api_key_for_agrees_with_the_resolver_for_every_provider() {
    let _lock = lock_test_env();
    let mut config = Config::default();
    config
        .providers
        .get_or_insert_with(Default::default)
        .custom
        .insert(
            "custom".into(),
            codewhale_runtime::config::ProviderConfig {
                kind: Some("openai-compatible".into()),
                base_url: Some("http://localhost:1234/v1".into()),
                model: Some("fixture-model".into()),
                ..Default::default()
            },
        );
    for provider in ProviderKind::all() {
        let identity = config.test_identity_for_kind(*provider);
        let resolution = resolve_credential_source(&config, &identity);
        assert_eq!(
            has_api_key_for(&config, &identity),
            resolution.is_present(),
            "{provider:?} disagreed: {:?}",
            resolution.source
        );
    }
}

/// #5772: with reuse off, an existing external CLI file must not be
/// stat'ed, read, or adopted — and the probe must not even claim whether
/// the candidate exists.
#[test]
fn unconsented_external_candidates_are_never_probed() {
    let _lock = lock_test_env();
    let temp = tempfile::tempdir().expect("external fixture");
    let codex_path = temp
        .path()
        .canonicalize()
        .expect("canonical temp root")
        .join("auth.json");
    std::fs::write(&codex_path, "{\"tokens\":{\"access_token\":\"x\"}}").expect("fixture");
    let _auth = EnvVarGuard::set("OPENAI_CODEX_AUTH_FILE", &codex_path);
    let _access = EnvVarGuard::remove("OPENAI_CODEX_ACCESS_TOKEN");
    let _legacy_access = EnvVarGuard::remove("CODEX_ACCESS_TOKEN");
    let _cli_key = EnvVarGuard::remove("CODEWHALE_CLI_API_KEY");
    let config = {
        let __cw_config_init_0_0 = Some("openai-codex".to_string());
        let mut __cw_config_init_0_value: Config = Config::default();
        __cw_config_init_0_value.provider = __cw_config_init_0_0;
        __cw_config_init_0_value
    };

    codewhale_runtime::external_credentials::reset_side_effect_trap();
    let resolution = resolve_credential_source(
        &config,
        &(config).test_identity_for_kind(ProviderKind::OpenaiCodex),
    );
    assert!(!resolution.is_present());
    assert!(!has_api_key_for(
        &config,
        &(config).test_identity_for_kind(ProviderKind::OpenaiCodex)
    ));
    let checked = resolution.checked_places();
    assert!(
        checked.contains("Codewhale-owned ChatGPT sign-in"),
        "the probe names the owned connection without accessing external state: {checked}"
    );
    assert!(
        !checked.contains("(absent)") && !checked.contains("present, not consented"),
        "no stat means no existence claim: {checked}"
    );
    assert_eq!(
        codewhale_runtime::external_credentials::complete_side_effect_trap_counts(),
        (0, 0, 0, 0, 0),
        "resolution must not touch external credential state"
    );
}

/// #5772: a consent record is not a credential. With a persisted consent
/// record whose pinned file is absent, resolution performs exactly the
/// read the user authorized — one secure open of the exact consented path —
/// and resolves as *missing* rather than masquerading as a stored
/// credential. No write, refresh, or network side effect is permitted.
#[test]
fn chatgpt_resolution_does_not_import_even_consented_external_files() {
    let _lock = lock_test_env();
    let temp = tempfile::tempdir().expect("external fixture");
    let codex_path = temp
        .path()
        .canonicalize()
        .expect("canonical temp root")
        .join("absent-auth.json");
    let _auth = EnvVarGuard::set("OPENAI_CODEX_AUTH_FILE", &codex_path);
    let _access = EnvVarGuard::remove("OPENAI_CODEX_ACCESS_TOKEN");
    let _legacy_access = EnvVarGuard::remove("CODEX_ACCESS_TOKEN");
    let _cli_key = EnvVarGuard::remove("CODEWHALE_CLI_API_KEY");
    let config = {
        let __cw_config_init_1_0 = Some("openai-codex".to_string());
        let __cw_config_init_1_1 = Some(ProvidersConfig {
            openai_codex: ProviderConfig {
                auth_mode: Some("oauth".to_string()),
                external_credentials: Some(
                    codewhale_config::ExternalCredentialConsentToml::read_only(
                        codewhale_config::ProviderKind::OpenaiCodex,
                        codewhale_config::ExternalCredentialSource::CodexCli,
                        codex_path.clone(),
                    ),
                ),
                ..ProviderConfig::default()
            },
            ..ProvidersConfig::default()
        });
        let mut __cw_config_init_1_value: Config = Config::default();
        __cw_config_init_1_value.provider = __cw_config_init_1_0;
        __cw_config_init_1_value.providers = __cw_config_init_1_1;
        __cw_config_init_1_value
    };

    codewhale_runtime::external_credentials::reset_side_effect_trap();
    let resolution = resolve_credential_source(
        &config,
        &(config).test_identity_for_kind(ProviderKind::OpenaiCodex),
    );
    assert!(
        !resolution.is_present(),
        "a consent record whose file is gone must resolve as missing: {:?}",
        resolution.source
    );
    assert!(
        resolution
            .checked_places()
            .contains("Codewhale-owned ChatGPT sign-in"),
        "the probe requires Codewhale sign-in despite external consent: {}",
        resolution.checked_places()
    );
    assert_eq!(
        codewhale_runtime::external_credentials::complete_side_effect_trap_counts(),
        (0, 0, 0, 0, 0),
        "external consent cannot authorize the public ChatGPT API"
    );
    assert!(!has_api_key_for(
        &config,
        &(config).test_identity_for_kind(ProviderKind::OpenaiCodex)
    ));
    assert_eq!(
        codewhale_runtime::external_credentials::complete_side_effect_trap_counts(),
        (0, 0, 0, 0, 0),
        "has_api_key_for never reads another client credential"
    );
}

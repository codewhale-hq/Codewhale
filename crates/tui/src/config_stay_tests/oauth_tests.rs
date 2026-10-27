use codewhale_runtime::config::Config;

use codewhale_config::ProviderKind;

use codewhale_runtime::oauth::*;

use std::fs;

// ────────────────────────────────────────────────────────────────────
// Ported from the deleted per-provider modules (§D security pins).
// ────────────────────────────────────────────────────────────────────

use tempfile::TempDir;

/// #4763 root trigger, re-pinned for #5772. A returning xAI-OAuth user
/// whose only material is an external Grok CLI grant loses readiness the
/// moment that CLI's short-lived access token expires, even though a
/// refresh token sits right beside it — read-only consent deliberately
/// never refreshes or rewrites another CLI's file, so there is nothing to
/// renew it with. `needs_api_key` therefore flips to true and onboarding
/// reopens. That is the intended invariant, not a leak, and it is what
/// stops a surviving consent record from reading as a stored credential;
/// this test pins it so the onboarding entry point stays explainable.
#[test]
fn expired_external_grok_grant_reads_as_missing_key_despite_refresh_token() {
    let _guard = crate::test_support::lock_test_env();
    let dir = TempDir::new().unwrap();
    let root = dir.path().canonicalize().expect("canonical temp root");
    let path = root.join("external-grok-auth.json");
    let scope = format!("https://auth.x.ai::{GROK_OIDC_CLIENT_ID}");
    fs::write(
        &path,
        serde_json::json!({
            scope.clone(): {
                "key": "expired-external-access",
                "refresh_token": "present-but-unusable-under-read-only-consent",
                "expires_at": (chrono::Utc::now() - chrono::Duration::hours(1)).to_rfc3339(),
                "oidc_client_id": GROK_OIDC_CLIENT_ID,
            }
        })
        .to_string(),
    )
    .unwrap();
    let _home_guard =
        crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", root.join("codewhale-owned"));
    let _path_guard = crate::test_support::EnvVarGuard::set("GROK_AUTH_PATH", &path);
    let _key_guard = crate::test_support::EnvVarGuard::remove("XAI_API_KEY");
    let config = {
        let __cw_config_init_0_0 = Some(ProviderKind::Xai.as_str().to_string());
        let __cw_config_init_0_1 = Some(codewhale_runtime::config::ProvidersConfig {
            xai: codewhale_runtime::config::ProviderConfig {
                auth_mode: Some("oauth".to_string()),
                external_credentials: Some(
                    codewhale_config::ExternalCredentialConsentToml::read_only(
                        codewhale_config::ProviderKind::Xai,
                        codewhale_config::ExternalCredentialSource::GrokCli,
                        path.clone(),
                    ),
                ),
                ..Default::default()
            },
            ..Default::default()
        });
        let mut __cw_config_init_0_value: Config = Default::default();
        __cw_config_init_0_value.provider = __cw_config_init_0_0;
        __cw_config_init_0_value.providers = __cw_config_init_0_1;
        __cw_config_init_0_value
    };

    codewhale_runtime::external_credentials::reset_side_effect_trap();
    assert!(
        !credentials_present(OAuthProvider::Xai, &config),
        "an expired external access token is not usable material"
    );
    assert!(
        !crate::provider_readiness::has_api_key_for(
            &config,
            &(config).test_identity_for_kind(ProviderKind::Xai)
        ),
        "expired external xAI OAuth must fall through to the missing-key path"
    );
    assert_eq!(
        codewhale_runtime::external_credentials::complete_side_effect_trap_counts().2,
        0,
        "a consented read never rewrites Codewhale-owned storage"
    );
    assert_eq!(
        codewhale_runtime::external_credentials::complete_side_effect_trap_counts().3,
        0,
        "a consented read never refreshes another CLI's token"
    );

    // The same file with a live access token is ready, so the check is
    // expiry-driven rather than a blanket rejection of external grants.
    fs::write(
        &path,
        serde_json::json!({
            scope: {
                "key": "fresh-external-access",
                "refresh_token": "unused",
                "expires_at": (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339(),
                "oidc_client_id": GROK_OIDC_CLIENT_ID,
            }
        })
        .to_string(),
    )
    .unwrap();
    assert!(credentials_present(OAuthProvider::Xai, &config));
    assert!(crate::provider_readiness::has_api_key_for(
        &config,
        &(config).test_identity_for_kind(ProviderKind::Xai)
    ));
}
#[test]
fn siwc_refresh_rotates_atomically_and_rejects_a_different_account() {
    codewhale_runtime::oauth::test_support::siwc_refresh_rotates_atomically_and_rejects_a_different_account(|config| crate::client::CodewhaleClient::new(config).unwrap().chatgpt_reasoning_api);
}

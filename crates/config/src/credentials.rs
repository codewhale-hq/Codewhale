//! Canonical provider-credential writes shared by the CLI (`auth set`),
//! the runtime API secret route, and any future host. Owning this here keeps
//! every writer on the same transactional discipline: snapshot the prior
//! secret, write the durable backend, refuse plaintext config fallback, and
//! roll both stores back if either leg fails.

use anyhow::{Context, Result};
use sha2::Digest as _;

use crate::provider_kind::ProviderKind;
use crate::{ConfigStore, Secrets};

/// Resolve the store for credential-adjacent writes: provider selection,
/// `auth_mode` markers, and the plaintext-free metadata that accompanies a
/// saved key.
///
/// Credentials and their metadata are user-global — a key saved while
/// working in one repo must be visible from every other repo, and the secret
/// store already is. When the ambient config path is a workspace-scoped
/// document (`<repo>/.codewhale/config.toml`), credential writes must not
/// bind the provider or write auth markers there: the binding would be
/// invisible from every other repo and would invite plaintext keys into a
/// committable repo file. Returns a store loaded on the user-global document
/// in that case, or `None` when the ambient store is already correctly
/// scoped, so key + provider binding + auth markers share one user-global
/// scope by default.
pub fn credential_metadata_store(store: &ConfigStore) -> Result<Option<ConfigStore>> {
    if !crate::config_path_is_workspace_scoped(store.path()) {
        return Ok(None);
    }
    let global = crate::default_config_path()?;
    ConfigStore::load(Some(global)).map(Some)
}

/// The secret-store slot a provider's key occupies. Shared-account families
/// (SiliconFlow China, the Model Studio variants) collapse onto one slot;
/// see [`ProviderKind::secret_store_slot`].
#[must_use]
pub fn provider_slot(provider: ProviderKind) -> &'static str {
    provider.secret_store_slot()
}

/// Remove any plaintext `api_key` left in the config for `provider`.
pub fn clear_provider_api_key_from_config(store: &mut ConfigStore, provider: ProviderKind) {
    store.config.providers.for_provider_mut(provider).api_key = None;
}

/// Plaintext-free metadata that accompanies a saved key.
///
/// Saving a credential never writes a model: every provider resolves an
/// unset model to its own default, and model choice belongs to the
/// model/config commands.
pub fn prepare_provider_api_key_metadata(store: &mut ConfigStore, provider: ProviderKind) {
    // The root `auth_mode` is the active provider's fallback marker. Writing
    // it for an inactive provider would leak `api_key` onto the active route
    // (e.g. a keyless local Ollama), so only the saved provider's own table
    // is marked unless it is the active one.
    if provider == store.config.provider {
        store.config.auth_mode = Some("api_key".to_string());
    }
    let provider_config = store.config.providers.for_provider_mut(provider);
    provider_config.auth_mode = Some("api_key".to_string());
    provider_config.external_credentials = None;
    if matches!(provider, ProviderKind::Xai | ProviderKind::Anthropic) {
        provider_config.oauth_credential_generation = None;
    }
}

/// ChatGPT plan access uses Codewhale's issued OAuth registration. A saved
/// API key would belong to a different billing route.
pub const OPENAI_CODEX_API_KEY_REFUSAL: &str = "Sign in with ChatGPT via `codewhale auth chatgpt` to use your plan allowance with Codewhale-owned credentials. Use the openai provider for a separately billed API key. Codewhale does not store an API key for this provider.";

/// Persist a provider credential to the durable secret store without silently
/// downgrading a backend failure to plaintext config storage.
///
/// Returns `true` when the key landed in the secret store (config then holds
/// metadata only). Callers must not print or echo `api_key`.
pub fn set_provider_api_key(
    store: &mut ConfigStore,
    secrets: &Secrets,
    provider: ProviderKind,
    api_key: &str,
) -> Result<bool> {
    anyhow::ensure!(
        provider != ProviderKind::OpenaiCodex,
        OPENAI_CODEX_API_KEY_REFUSAL
    );
    // #6528: strip pasted invisible characters and whitespace in one place.
    let api_key = codewhale_secrets::normalize_api_key(api_key);
    anyhow::ensure!(!api_key.is_empty(), "Refusing to save an empty API key.");
    let api_key = api_key.as_str();
    if provider == ProviderKind::Xai {
        return crate::with_xai_oauth_revocation_transaction(|| {
            set_provider_api_key_unlocked(store, secrets, provider, api_key)
        });
    }
    if provider == ProviderKind::Anthropic {
        return crate::with_xai_oauth_lifecycle_lock(|_| {
            let saved = set_provider_api_key_unlocked(store, secrets, provider, api_key)?;
            crate::clear_all_claude_oauth_credentials_locked().context(
                "API-key billing was selected and saved, but old Claude sign-in cleanup failed",
            )?;
            Ok(saved)
        });
    }
    set_provider_api_key_unlocked(store, secrets, provider, api_key)
}

fn set_provider_api_key_unlocked(
    store: &mut ConfigStore,
    secrets: &Secrets,
    provider: ProviderKind,
    api_key: &str,
) -> Result<bool> {
    let original_config = store.config.clone();
    prepare_provider_api_key_metadata(store, provider);
    let slot = provider_slot(provider);
    // A readable prior value is required before a secret-store write so a
    // later config failure can restore the exact prior state. If the backend
    // cannot provide that snapshot, fail before changing the config file.
    let prior_secret = secrets.get(slot);
    let secret_store_saved = match prior_secret.as_ref().map_err(|error| error.to_string()) {
        Ok(_) => match secrets.set(slot, api_key) {
            Ok(()) => {
                clear_provider_api_key_from_config(store, provider);
                true
            }
            Err(err) => {
                store.config = original_config;
                return Err(anyhow::anyhow!(
                    "Secret storage write failed for {slot}: {err}. Refusing to write the API key in plaintext to {}. Fix the configured secret backend and retry; Codewhale did not change that file.",
                    crate::quote_os_path(store.path())
                ));
            }
        },
        Err(error) => {
            store.config = original_config;
            return Err(anyhow::anyhow!(
                "Secret storage snapshot failed for {slot}: {error}. Refusing to write the API key in plaintext to {}. Fix the configured secret backend and retry; Codewhale did not change that file.",
                crate::quote_os_path(store.path())
            ));
        }
    };
    if let Err(error) = store.save() {
        store.config = original_config;
        if secret_store_saved {
            let current = secrets
                .get(slot)
                .map_err(|rollback| anyhow::anyhow!(
                    "{error}; additionally could not verify secret-store rollback for {slot}: {rollback}"
                ))?;
            if current.as_deref() == Some(api_key) {
                match prior_secret.expect("snapshot succeeded before secret write") {
                    Some(previous) => secrets.set(slot, &previous),
                    None => secrets.delete(slot),
                }
                .map_err(|rollback| anyhow::anyhow!(
                    "{error}; additionally failed to restore prior secret-store state for {slot}: {rollback}"
                ))?;
            }
        }
        return Err(error);
    }
    crate::scrub_plaintext_api_keys_from_config_backup(store.path())
        .context("failed to scrub plaintext API keys from config backup")?;
    Ok(secret_store_saved)
}

/// What a credential clear actually accomplished.
///
/// The secret-store leg can fail after the config leg has already been
/// persisted. Reporting that separately is the point: a caller that prints
/// "cleared" while the key is still sitting in the keyring has lied about a
/// security-relevant action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClearOutcome {
    /// The secret-store slot the clear targeted.
    pub slot: &'static str,
    /// `None` when the secret store accepted the delete or holds no key for the
    /// slot; otherwise the backend error, already stringified so it carries no
    /// credential material.
    pub secret_store_error: Option<String>,
}

impl ClearOutcome {
    /// True only when both the config and the secret store were cleared.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.secret_store_error.is_none()
    }
}

/// Remove a provider credential from config and the durable secret store.
///
/// Shared by `codewhale auth clear` and the runtime API's credential route so
/// both get the same ordering and the same rollback: the config document is
/// snapshotted and restored if its save fails, and the secret store is only
/// touched once the config write has landed. A secret-store failure is
/// returned rather than swallowed, because the config no longer advertises a
/// key that the backend may still hold.
///
/// This deliberately does not clear external-consent or environment-sourced
/// credentials: Codewhale does not own those, and a caller must refuse the
/// request instead of implying it revoked something it cannot reach.
pub fn clear_provider_api_key(
    store: &mut ConfigStore,
    secrets: &Secrets,
    provider: ProviderKind,
) -> Result<ClearOutcome> {
    let slot = provider_slot(provider);
    let original_config = store.config.clone();
    clear_provider_api_key_from_config(store, provider);
    // Only xAI carries OAuth generation and consent state alongside the key,
    // and `codewhale auth clear` has always cleared those three together. Every
    // other provider keeps its `auth_mode` marker deliberately: the route is
    // still an API-key route, it simply has no key now, which is exactly the
    // `missing` credential state a client needs to see.
    if provider == ProviderKind::Xai {
        let xai = store.config.providers.for_provider_mut(provider);
        xai.oauth_credential_generation = None;
        xai.auth_mode = None;
        xai.external_credentials = None;
    }
    if let Err(error) = store.save() {
        store.config = original_config;
        return Err(error);
    }
    // A backend that refuses every delete (a read-only store) but holds no key
    // for this slot has nothing left to revoke, so that refusal is not a
    // failure. Both callers get this rule from here.
    let secret_store_error = secrets
        .delete(slot)
        .err()
        .filter(|_| !matches!(secrets.get(slot), Ok(None)))
        .map(|error| error.to_string());
    Ok(ClearOutcome {
        slot,
        secret_store_error,
    })
}

/// One-way digest proving a credential (and the endpoint it is bound to) is
/// still the same one.
///
/// SHA-256 over a domain-separated, length-prefixed preimage, truncated to 128
/// bits. Length prefixing keeps `(base_url, key)` unambiguous, so no pair of
/// distinct routes can be made to share a generation by moving bytes across the
/// boundary. The value is never rendered: it is credential-derived, and a
/// stable public digest of a secret is a secret's shadow.
#[derive(Clone, PartialEq, Eq)]
pub struct CredentialGeneration(String);

impl CredentialGeneration {
    #[must_use]
    pub fn derive(base_url: &str, credential: &str) -> Self {
        let mut hasher = sha2::Sha256::new();
        hasher.update(b"codewhale/turn-route/credential-generation/v1\0");
        hasher.update(
            u64::try_from(base_url.len())
                .unwrap_or(u64::MAX)
                .to_le_bytes(),
        );
        hasher.update(base_url.as_bytes());
        hasher.update(
            u64::try_from(credential.len())
                .unwrap_or(u64::MAX)
                .to_le_bytes(),
        );
        hasher.update(credential.as_bytes());
        let digest = hasher.finalize();
        let mut hex = String::with_capacity(32);
        for byte in &digest[..16] {
            use std::fmt::Write as _;
            let _ = write!(hex, "{byte:02x}");
        }
        Self(hex)
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// Redacted: see the type docs. There is no accessor for the digest string.
impl std::fmt::Debug for CredentialGeneration {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("<redacted>")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codewhale_secrets::{KeyringStore, SecretsError};
    use std::sync::Arc;

    /// A store that refuses every delete and holds `.0` for every slot.
    struct UndeletableStore(Option<&'static str>);

    impl KeyringStore for UndeletableStore {
        fn get(&self, _key: &str) -> Result<Option<String>, SecretsError> {
            Ok(self.0.map(str::to_string))
        }

        fn set(&self, _key: &str, _value: &str) -> Result<(), SecretsError> {
            Err(SecretsError::ReadOnly)
        }

        fn delete(&self, _key: &str) -> Result<(), SecretsError> {
            Err(SecretsError::Keyring("test delete failure".to_string()))
        }

        fn backend_name(&self) -> &'static str {
            "undeletable test store"
        }
    }

    #[test]
    fn a_refused_delete_fails_the_clear_only_while_the_store_still_holds_a_key() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        for (held, fails) in [(Some("sk-keyring-fixture"), true), (None, false)] {
            let mut store = ConfigStore::load(Some(path.clone())).expect("load config");
            store.config.providers.deepseek.api_key = Some("sk-config-fixture".to_string());
            let secrets = Secrets::new(Arc::new(UndeletableStore(held)));
            let outcome = clear_provider_api_key(&mut store, &secrets, ProviderKind::Deepseek)
                .expect("the config leg saves");
            assert!(store.config.providers.deepseek.api_key.is_none());
            assert_eq!(outcome.is_complete(), !fails, "delete refusal completion");
            if let Some(error) = outcome.secret_store_error {
                assert!(
                    !error.contains("sk-keyring-fixture"),
                    "credential leaked into error"
                );
            }
        }
    }

    fn store_with(body: &str) -> (tempfile::TempDir, ConfigStore) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        std::fs::write(&path, body).expect("seed config");
        let store = ConfigStore::load(Some(path)).expect("store loads");
        (dir, store)
    }

    fn in_memory_secrets() -> Secrets {
        Secrets::new(std::sync::Arc::new(
            codewhale_secrets::InMemoryKeyringStore::new(),
        ))
    }

    #[test]
    fn saving_a_key_for_an_inactive_provider_leaves_root_auth_mode_alone() {
        let (_dir, mut store) = store_with("provider = \"ollama\"\n");
        let secrets = in_memory_secrets();

        set_provider_api_key(&mut store, &secrets, ProviderKind::Openrouter, "or-key")
            .expect("save succeeds");

        let saved = ConfigStore::load(Some(store.path().to_path_buf())).expect("reload");
        assert_eq!(saved.config.provider, ProviderKind::Ollama);
        assert_eq!(saved.config.auth_mode, None);
        assert_eq!(
            saved.config.providers.openrouter.auth_mode.as_deref(),
            Some("api_key")
        );
    }

    #[test]
    fn saving_a_key_for_the_active_provider_still_marks_root_auth_mode() {
        let (_dir, mut store) = store_with("provider = \"openrouter\"\n");
        let secrets = in_memory_secrets();

        set_provider_api_key(&mut store, &secrets, ProviderKind::Openrouter, "or-key")
            .expect("save succeeds");

        let saved = ConfigStore::load(Some(store.path().to_path_buf())).expect("reload");
        assert_eq!(saved.config.auth_mode.as_deref(), Some("api_key"));
    }

    #[test]
    fn openai_codex_key_save_is_refused_and_keeps_external_consent() {
        let (_dir, mut store) = store_with(
            "provider = \"openai-codex\"\n\n[providers.openai-codex]\nauth_mode = \"oauth\"\n",
        );
        store.config.providers.openai_codex.external_credentials =
            Some(crate::ExternalCredentialConsentToml::read_only(
                ProviderKind::OpenaiCodex,
                crate::ExternalCredentialSource::CodexCli,
                std::path::PathBuf::from("/synthetic/codex/auth.json"),
            ));
        store.save().expect("seed consent");
        let before = std::fs::read_to_string(store.path()).expect("config before");
        let secrets = in_memory_secrets();

        let error = set_provider_api_key(&mut store, &secrets, ProviderKind::OpenaiCodex, "k")
            .expect_err("openai-codex keys are not stored");

        assert!(
            error.to_string().contains("Sign in with ChatGPT"),
            "{error}"
        );
        assert_eq!(
            std::fs::read_to_string(store.path()).expect("config after"),
            before
        );
        assert!(
            store
                .config
                .providers
                .openai_codex
                .external_credentials
                .is_some()
        );
        assert_eq!(
            secrets
                .get(provider_slot(ProviderKind::OpenaiCodex))
                .expect("read"),
            None
        );
    }
}
